use alloc::{collections::BTreeMap, sync::Arc, vec::Vec};

use ax_errno::{AxError, AxResult, LinuxError};
use ax_runtime::hal::time::monotonic_time_nanos;
use ax_sync::Mutex;
use ax_task::current;
use bytemuck::AnyBitPattern;
use linux_raw_sys::general::*;
use starry_process::Pid;
use starry_vm::{VmMutPtr, VmPtr, vm_load, vm_write_slice};

use super::{
    IPC_CREAT, IPC_EXCL, IPC_INFO, IPC_PRIVATE, IPC_RMID, IPC_SET, IPC_STAT, IpcPerm, MSG_INFO,
    MSG_STAT, has_ipc_permission, next_ipc_id,
};
use crate::task::{AsThread, WaitQueue as MsgWaitQueue};

/// Data structure describing a message queue.
#[repr(C)]
#[derive(Clone, Copy, AnyBitPattern)]
#[allow(non_camel_case_types)]
pub struct msqid_ds {
    /// operation permission struct
    pub msg_perm: IpcPerm,
    /// time of last msgsnd()
    pub msg_stime: __kernel_time_t,
    /// time of last msgrcv()
    pub msg_rtime: __kernel_time_t,
    /// time of last change by msgctl()
    pub msg_ctime: __kernel_time_t,
    /// current number of bytes on queue
    pub msg_cbytes: __kernel_size_t,
    /// number of messages in queue
    pub msg_qnum: __kernel_size_t,
    /// max number of bytes on queue
    pub msg_qbytes: __kernel_size_t,
    /// pid of last msgsnd()
    pub msg_lspid: __kernel_pid_t,
    /// pid of last msgrcv()
    pub msg_lrpid: __kernel_pid_t,
}

impl msqid_ds {
    fn new(key: i32, mode: __kernel_mode_t, pid: __kernel_pid_t, uid: u32, gid: u32) -> Self {
        Self {
            msg_perm: IpcPerm {
                key,
                uid,
                gid,
                cuid: uid,
                cgid: gid,
                mode,
                seq: 0,
                pad: 0,
                unused0: 0,
                unused1: 0,
            },
            msg_stime: 0,
            msg_rtime: 0,
            msg_ctime: monotonic_time_nanos() as __kernel_time_t,
            msg_cbytes: 0,
            msg_qnum: 0,
            msg_qbytes: MSGMNB as __kernel_size_t,
            msg_lspid: pid,
            msg_lrpid: pid,
        }
    }
}

/// Single message in the queue
pub struct Message {
    /// message type
    pub mtype: i64,
    /// message data
    pub data: Vec<u8>,
    /// FIFO sequence number
    pub seq: u64,
}

/// This struct is used to maintain the message queue in kernel.
pub struct MessageQueue {
    /// Message queue data structure
    pub msqid_ds: msqid_ds,
    /// Queue of messages
    pub messages: BTreeMap<i64, Vec<Message>>, // mtype -> messages of that type
    /// Total bytes in queue
    pub total_bytes: usize,
    /// Next FIFO sequence number
    pub next_seq: u64,
    /// Waiters blocked on send due to full queue
    pub send_wait_queue: Arc<MsgWaitQueue>,
    /// Waiters blocked on receive due to empty queue
    pub recv_wait_queue: Arc<MsgWaitQueue>,
    /// Marked for removal
    pub mark_removed: bool,
    /// IPC namespace ID that owns this queue
    pub ns_id: u64,
}

impl MessageQueue {
    /// Creates a new [`MessageQueue`].
    #[allow(clippy::too_many_arguments)]
    pub fn new(key: i32, mode: __kernel_mode_t, pid: Pid, uid: u32, gid: u32, ns_id: u64) -> Self {
        MessageQueue {
            msqid_ds: msqid_ds::new(key, mode, pid as __kernel_pid_t, uid, gid),
            messages: BTreeMap::new(),
            total_bytes: 0,
            next_seq: 0,
            send_wait_queue: Arc::new(MsgWaitQueue::new()),
            recv_wait_queue: Arc::new(MsgWaitQueue::new()),
            mark_removed: false,
            ns_id,
        }
    }

    /// Add a message to the queue
    pub fn enqueue_message(&mut self, mtype: i64, data: Vec<u8>) -> AxResult<()> {
        let data_len = data.len();
        // Check queue size limits
        if self.total_bytes + data_len > self.msqid_ds.msg_qbytes as usize {
            return Err(AxError::from(LinuxError::ENOSPC)); // ENOSPC
        }

        let seq = self.next_seq;
        self.next_seq = self.next_seq.wrapping_add(1);
        let message = Message { mtype, data, seq };

        self.messages.entry(mtype).or_default().push(message);
        self.total_bytes += data_len;
        self.msqid_ds.msg_cbytes += data_len as __kernel_size_t;
        self.msqid_ds.msg_qnum += 1;

        Ok(())
    }

    fn find_fifo_message<F>(&self, filter: F) -> Option<(i64, &[u8])>
    where
        F: Fn(i64) -> bool,
    {
        let mut candidate: Option<&Message> = None;

        for (&mtype, messages) in &self.messages {
            if !filter(mtype) {
                continue;
            }
            if let Some(message) = messages.first() {
                let should_pick = candidate
                    .map(|current| message.seq < current.seq)
                    .unwrap_or(true);
                if should_pick {
                    candidate = Some(message);
                }
            }
        }

        candidate.map(|message| (message.mtype, &message.data[..]))
    }

    /// Find the first message in FIFO order (without removing)
    pub fn find_first_message(&self) -> Option<(i64, &[u8])> {
        self.find_fifo_message(|_| true)
    }

    /// Find message by type (without removing)
    pub fn find_message_by_type(&self, msgtyp: i64) -> Option<(i64, &[u8])> {
        self.messages
            .get(&msgtyp)
            .and_then(|msgs| msgs.first())
            .map(|msg| (msgtyp, &msg.data[..]))
    }

    /// Find the first message with a type not equal to the specified value,
    /// in FIFO order (without removing)
    pub fn find_message_not_equal(&self, msgtyp: i64) -> Option<(i64, &[u8])> {
        self.find_fifo_message(|mtype| mtype != msgtyp)
    }

    /// Find the first message with a type less than or equal to |msgtyp|
    /// (without removing)
    pub fn find_message_less_equal(&self, abs_typ: i64) -> Option<(i64, &[u8])> {
        let mut candidate_type = None;

        // Find the smallest type among all types ≤ abs_typ
        for (&mtype, messages) in &self.messages {
            if mtype <= abs_typ
                && !messages.is_empty()
                && candidate_type.is_none_or(|candidate| mtype < candidate)
            {
                candidate_type = Some(mtype);
            }
        }

        // Return the found message (without removing)
        if let Some(mtype) = candidate_type {
            self.messages
                .get(&mtype)
                .and_then(|msgs| msgs.first())
                .map(|msg| (mtype, &msg.data[..]))
        } else {
            None
        }
    }

    /// Get total number of messages in the queue (for MSG_COPY)
    pub fn get_total_message_count(&self) -> usize {
        self.messages.values().map(|msgs| msgs.len()).sum()
    }

    /// Get message by FIFO index (for MSG_COPY)
    pub fn get_message_by_index(&self, index: usize) -> Option<&Message> {
        let total = self.get_total_message_count();
        if index >= total {
            return None;
        }

        let mut ordered: Vec<&Message> = Vec::with_capacity(total);
        for messages in self.messages.values() {
            for message in messages {
                ordered.push(message);
            }
        }
        let (_, selected, _) = ordered.select_nth_unstable_by_key(index, |message| message.seq);
        Some(*selected)
    }

    /// Remove the message by specified type and index
    pub fn remove_message_by_type_and_index(
        &mut self,
        mtype: i64,
        index: usize,
    ) -> AxResult<Message> {
        if let Some(messages) = self.messages.get_mut(&mtype)
            && index < messages.len()
        {
            let removed_msg = messages.remove(index);

            // Update core queue statistics in the removal method
            self.total_bytes -= removed_msg.data.len();
            self.msqid_ds.msg_cbytes -= removed_msg.data.len() as __kernel_size_t;
            self.msqid_ds.msg_qnum -= 1;

            // If the message list of this type is empty, remove the entire type entry
            if messages.is_empty() {
                self.messages.remove(&mtype);
            }

            return Ok(removed_msg);
        }

        Err(AxError::from(LinuxError::ENOMSG)) // ENOMSG
    }
}

fn queue_would_exceed(queue: &MessageQueue, data_len: usize) -> bool {
    let would_exceed_bytes = queue.total_bytes + data_len > queue.msqid_ds.msg_qbytes as usize;
    let would_exceed_messages =
        (queue.msqid_ds.msg_qnum + 1) as usize > queue.msqid_ds.msg_qbytes as usize;
    would_exceed_bytes || would_exceed_messages
}

fn find_matching_message<'a>(
    queue: &'a MessageQueue,
    msgtyp: i64,
    flags: &MsgRcvFlags,
) -> Option<(i64, &'a [u8])> {
    match msgtyp {
        0 => queue.find_first_message(),
        typ if typ > 0 => {
            if flags.contains(MsgRcvFlags::MSG_EXCEPT) {
                queue.find_message_not_equal(typ)
            } else {
                queue.find_message_by_type(typ)
            }
        }
        typ if typ < 0 => {
            let abs_typ = typ.abs();
            queue.find_message_less_equal(abs_typ)
        }
        _ => None,
    }
}

/// Message queue manager
pub struct MsgManager {
    /// (key, ns_id) -> msqid mapping
    key_msqid: BTreeMap<(i32, u64), i32>,
    /// msqid -> message queue structure
    msqid_queues: BTreeMap<i32, Arc<Mutex<MessageQueue>>>,
}

impl MsgManager {
    const fn new() -> Self {
        MsgManager {
            key_msqid: BTreeMap::new(),
            msqid_queues: BTreeMap::new(),
        }
    }

    /// Returns an iterator over all message queues
    pub fn iter_msg_queues(&self) -> impl Iterator<Item = (i32, &Arc<Mutex<MessageQueue>>)> {
        self.msqid_queues.iter().map(|(&k, v)| (k, v))
    }

    /// Returns an iterator over all message queues, filtering out removed ones
    pub fn iter_active_queues(&self) -> impl Iterator<Item = (i32, &Arc<Mutex<MessageQueue>>)> {
        self.iter_msg_queues().filter(|(_, queue)| {
            let guard = queue.lock();
            !guard.mark_removed
        })
    }

    /// Returns the message queue ID associated with the given key.
    pub fn get_msqid_by_key(&self, key: i32, ns_id: u64) -> Option<i32> {
        self.key_msqid.get(&(key, ns_id)).cloned()
    }

    /// Returns the message queue associated with the given ID, validating
    /// that it belongs to the specified IPC namespace.
    pub fn get_queue_by_msqid(&self, msqid: i32, ns_id: u64) -> Option<Arc<Mutex<MessageQueue>>> {
        self.msqid_queues
            .get(&msqid)
            .filter(|q| q.lock().ns_id == ns_id)
            .cloned()
    }

    /// Inserts a mapping from a key to a message queue ID.
    pub fn insert_key_msqid(&mut self, key: i32, ns_id: u64, msqid: i32) {
        self.key_msqid.insert((key, ns_id), msqid);
    }

    /// Inserts a mapping from a message queue ID to its queue.
    pub fn insert_msqid_queues(&mut self, msqid: i32, msg_queue: Arc<Mutex<MessageQueue>>) {
        self.msqid_queues.insert(msqid, msg_queue);
    }

    /// Returns the current number of message queues.
    pub fn queue_count(&self) -> usize {
        self.msqid_queues.len()
    }

    /// Remove a message queue
    pub fn remove_msqid(&mut self, msqid: i32) {
        self.key_msqid.retain(|_, &mut v| v != msqid);
        self.msqid_queues.remove(&msqid);
    }
}

/// System limits
/// Maximum number of message queues
pub const MSGMNI: usize = 32000;
/// Maximum bytes in a message queue
pub const MSGMNB: usize = 16384;
/// Maximum size of a single message
pub const MSGMAX: usize = 8192;

/// Global message queue manager
pub static MSG_MANAGER: Mutex<MsgManager> = Mutex::new(MsgManager::new());

bitflags::bitflags! {
    /// Flags for msgrcv
    #[derive(Debug)]
    pub struct MsgRcvFlags: i32 {
        /// Non-blocking receive (return immediately if no message)
        const IPC_NOWAIT = 0o4000;
        /// Truncate message if too long (instead of failing)
        const MSG_NOERROR = 0o10000;
        /// For internal use - mark as COPIED
        const MSG_COPY = 0o40000;
        /// Receive any message except of specified type (Linux extension)
        const MSG_EXCEPT = 0o20000;
    }
}

bitflags::bitflags! {
    /// Flags for msgsnd
    #[derive(Debug)]
    pub struct MsgSndFlags: i32 {
        /// Non-blocking send (return immediately if queue full)
        const IPC_NOWAIT = 0o4000;
    }
}

#[repr(C)]
#[derive(Debug, Copy, Clone)]
pub struct UserMsgbuf {
    pub mtype: i64,     // type of message
    pub mtext: [u8; 0], // actual data, use zero-sized array to simulate flexible array
}

pub fn sys_msgget(key: i32, msgflg: i32) -> AxResult<isize> {
    let current = current();
    let thread = current.as_thread();
    let proc_data = &thread.proc_data;
    let cred = thread.cred();
    let current_uid = cred.euid;
    let current_gid = cred.egid;
    let current_pid = proc_data.proc.pid();
    let ns_id = proc_data.nsproxy.lock().ipc_ns.lock().ns_id;

    let mut msg_manager = MSG_MANAGER.lock();

    // Check system limit
    if msg_manager.queue_count() >= MSGMNI {
        return Err(AxError::from(LinuxError::ENOSPC)); // ENOSPC
    }

    // Handle IPC_PRIVATE (always create new queue)
    if key == IPC_PRIVATE {
        let msqid = next_ipc_id();
        let msg_queue = Arc::new(Mutex::new(MessageQueue::new(
            key,
            (msgflg & 0o777) as _,
            current_pid,
            current_uid,
            current_gid,
            ns_id,
        )));

        msg_manager.insert_msqid_queues(msqid, msg_queue);
        return Ok(msqid as isize);
    }

    // Look for existing message queue
    if let Some(msqid) = msg_manager.get_msqid_by_key(key, ns_id) {
        let msg_queue = msg_manager
            .get_queue_by_msqid(msqid, ns_id)
            .ok_or(AxError::from(LinuxError::ENOENT))?; // ENOENT

        let msg_queue = msg_queue.lock();

        // Check permissions
        if !has_ipc_permission(
            &msg_queue.msqid_ds.msg_perm,
            current_uid,
            current_gid,
            false,
        ) {
            return Err(AxError::from(LinuxError::EACCES)); // EACCES
        }

        // Check if marked for removal
        if msg_queue.mark_removed {
            return Err(AxError::from(LinuxError::EIDRM)); // EIDRM
        }

        // Check IPC_EXCL flag
        if (msgflg & IPC_EXCL) != 0 && (msgflg & IPC_CREAT) != 0 {
            return Err(AxError::from(LinuxError::EEXIST)); // EEXIST
        }

        return Ok(msqid as isize);
    }

    // Create new message queue
    if (msgflg & IPC_CREAT) == 0 {
        return Err(AxError::from(LinuxError::ENOENT)); // ENOENT
    }

    let msqid = next_ipc_id();
    let msg_queue = Arc::new(Mutex::new(MessageQueue::new(
        key,
        (msgflg & 0o777) as _,
        current_pid,
        current_uid,
        current_gid,
        ns_id,
    )));

    msg_manager.insert_key_msqid(key, ns_id, msqid);
    msg_manager.insert_msqid_queues(msqid, msg_queue);

    Ok(msqid as isize)
}

pub fn sys_msgsnd(
    msqid: i32,
    msgp: *const UserMsgbuf,
    msgsz: usize,
    msgflg: i32,
) -> AxResult<isize> {
    // MSGMAX = 8192
    if msgsz > MSGMAX {
        return Err(AxError::from(LinuxError::EINVAL)); // EINVAL
    }
    let current = current();
    let thread = current.as_thread();
    let proc_data = &thread.proc_data;
    let cred = thread.cred();
    let current_uid = cred.euid;
    let current_gid = cred.egid;
    let current_pid = proc_data.proc.pid();
    let flags = MsgSndFlags::from_bits_truncate(msgflg);

    let msg_queue_ref = {
        let msg_manager = MSG_MANAGER.lock();
        let ns_id = proc_data.nsproxy.lock().ipc_ns.lock().ns_id;
        msg_manager
            .get_queue_by_msqid(msqid, ns_id)
            .ok_or(AxError::from(LinuxError::EINVAL))? // EINVAL - queue does not exist
    };

    {
        let msg_queue = msg_queue_ref.lock();
        if !has_ipc_permission(
            &msg_queue.msqid_ds.msg_perm,
            current_uid as _,
            current_gid as _,
            true,
        ) {
            return Err(AxError::from(LinuxError::EACCES)); // EACCES
        }
        if msg_queue.mark_removed {
            return Err(AxError::from(LinuxError::EIDRM)); // EIDRM
        }
    }

    // read message from user space
    let mtype_ptr = unsafe { core::ptr::addr_of!((*msgp).mtype) };
    let mtype: i64 = mtype_ptr.vm_read()?;

    if mtype <= 0 {
        return Err(AxError::from(LinuxError::EINVAL)); // EINVAL - invalid message type
    }

    // read data part
    let mtext_ptr = unsafe { core::ptr::addr_of!((*msgp).mtext) };
    let data_vec = vm_load(mtext_ptr.cast::<u8>(), msgsz)?;
    let data_len = data_vec.len();

    loop {
        let mut msg_queue = msg_queue_ref.lock();
        if msg_queue.mark_removed {
            return Err(AxError::from(LinuxError::EIDRM)); // EIDRM
        }

        if !queue_would_exceed(&msg_queue, data_len) {
            msg_queue.enqueue_message(mtype, data_vec)?;
            msg_queue.msqid_ds.msg_lspid = current_pid as _;
            msg_queue.msqid_ds.msg_stime = monotonic_time_nanos() as _;

            let recv_wait_queue = msg_queue.recv_wait_queue.clone();
            drop(msg_queue);
            recv_wait_queue.wake(usize::MAX, u32::MAX);
            return Ok(0);
        }

        if flags.contains(MsgSndFlags::IPC_NOWAIT) {
            return Err(AxError::from(LinuxError::EAGAIN)); // EAGAIN
        }

        let send_wait_queue = msg_queue.send_wait_queue.clone();
        drop(msg_queue);
        let _ = send_wait_queue.wait_if(u32::MAX, None, || {
            let msg_queue = msg_queue_ref.lock();
            !msg_queue.mark_removed && queue_would_exceed(&msg_queue, data_len)
        })?;
    }
}

pub fn sys_msgrcv(
    msqid: i32,
    msgp: *mut UserMsgbuf,
    msgsz: usize,
    msgtyp: i64,
    msgflg: i32,
) -> AxResult<isize> {
    // Parse flags and get current process information

    let mut flags = MsgRcvFlags::from_bits_truncate(msgflg);
    const IPC_NOWAIT_RAW: i32 = 0o4000;
    const MSG_NOERROR_RAW: i32 = 0o10000;
    const MSG_EXCEPT_RAW: i32 = 0o20000;
    const MSG_COPY_RAW: i32 = 0o40000;
    let msg_copy = (msgflg & MSG_COPY_RAW) != 0;
    let msg_except = (msgflg & MSG_EXCEPT_RAW) != 0;
    let ipc_nowait = (msgflg & IPC_NOWAIT_RAW) != 0;
    let msg_noerror = (msgflg & MSG_NOERROR_RAW) != 0;
    if msg_except {
        flags |= MsgRcvFlags::MSG_EXCEPT;
    } else {
        flags.remove(MsgRcvFlags::MSG_EXCEPT);
    }
    let current = current();
    let thread = current.as_thread();
    let proc_data = &thread.proc_data;
    let cred = thread.cred();
    let current_uid = cred.euid;
    let current_gid = cred.egid;
    let current_pid = proc_data.proc.pid();

    // Check validity of flag combinations
    if msg_copy {
        if !ipc_nowait {
            return Err(AxError::from(LinuxError::EINVAL)); // EINVAL - MSG_COPY must be used with IPC_NOWAIT
        }
        if msg_except {
            return Err(AxError::from(LinuxError::EINVAL)); // EINVAL - MSG_COPY and MSG_EXCEPT are mutually exclusive
        }
    }
    if msgtyp == i64::MIN {
        return Err(AxError::from(LinuxError::EINVAL)); // EINVAL - invalid msgtyp (abs overflow)
    }
    // Get the message queue
    let msg_queue_ref = {
        let msg_manager = MSG_MANAGER.lock();
        let ns_id = proc_data.nsproxy.lock().ipc_ns.lock().ns_id;
        msg_manager
            .get_queue_by_msqid(msqid, ns_id)
            .ok_or(AxError::from(LinuxError::EINVAL))? // EINVAL
    };

    let mut msg_queue = msg_queue_ref.lock();

    // Permission check
    if !has_ipc_permission(
        &msg_queue.msqid_ds.msg_perm,
        current_uid as _,
        current_gid as _,
        false,
    ) {
        return Err(AxError::from(LinuxError::EACCES)); // EACCES
    }

    if msg_queue.mark_removed {
        return Err(AxError::from(LinuxError::EIDRM)); // EIDRM
    }

    // Message matching logic (distinguish between MSG_COPY and normal mode)
    let (mtype, data_slice, index, should_remove) = if msg_copy {
        // MSG_COPY mode: msgtyp is the message index
        if msgtyp < 0 {
            return Err(AxError::from(LinuxError::EINVAL)); // EINVAL - MSG_COPY requires non-negative index
        }
        let index = usize::try_from(msgtyp).map_err(|_| AxError::from(LinuxError::EINVAL))?; // EINVAL - index out of range

        // Check if the index is valid
        if index >= msg_queue.get_total_message_count() {
            return Err(AxError::from(LinuxError::ENOMSG)); // ENOMSG - index out of range
        }

        // Get a copy of the message (do not remove)
        let message = msg_queue
            .get_message_by_index(index)
            .ok_or(AxError::from(LinuxError::ENOMSG))?; // ENOMSG

        (message.mtype, &message.data[..], index, false) // should_remove = false
    } else {
        loop {
            if msg_queue.mark_removed {
                return Err(AxError::from(LinuxError::EIDRM)); // EIDRM
            }

            let matched_message = find_matching_message(&msg_queue, msgtyp, &flags);
            if let Some((mtype, data_slice)) = matched_message {
                // Index is always 0 in normal mode
                let index = 0;
                break (mtype, data_slice, index, true);
            }

            if ipc_nowait {
                return Err(AxError::from(LinuxError::ENOMSG)); // ENOMSG
            }

            let recv_wait_queue = msg_queue.recv_wait_queue.clone();
            drop(msg_queue);
            let _ = recv_wait_queue.wait_if(u32::MAX, None, || {
                let msg_queue = msg_queue_ref.lock();
                !msg_queue.mark_removed
                    && find_matching_message(&msg_queue, msgtyp, &flags).is_none()
            })?;
            msg_queue = msg_queue_ref.lock();
        }
    };

    // Message size check
    if data_slice.len() > msgsz {
        if msg_noerror {
            // MSG_NOERROR: Truncate the message and continue
        } else {
            // Without MSG_NOERROR: return an error
            // Note: If in normal mode, the message has not been removed, so no need to
            // restore
            return Err(AxError::from(LinuxError::E2BIG)); // E2BIG
        }
    }

    // Write mtype
    let mtype_ptr = unsafe { core::ptr::addr_of_mut!((*msgp).mtype) };
    mtype_ptr.vm_write(mtype)?;

    // Write data part
    let data_ptr = unsafe { core::ptr::addr_of_mut!((*msgp).mtext) };
    let copy_len = data_slice.len().min(msgsz);
    vm_write_slice(data_ptr.cast::<u8>(), &data_slice[..copy_len])?;

    // Remove the message from the queue (normal mode only)
    if should_remove {
        msg_queue.remove_message_by_type_and_index(mtype, index)?;
    }

    // Update queue statistics (normal mode only)
    if should_remove {
        msg_queue.msqid_ds.msg_lrpid = current_pid as _;
        msg_queue.msqid_ds.msg_rtime = monotonic_time_nanos() as _;
    } else {
        // MSG_COPY mode: only update last receiver info, do not update queue statistics
        msg_queue.msqid_ds.msg_lrpid = current_pid as _;
        msg_queue.msqid_ds.msg_rtime = monotonic_time_nanos() as _;
    }

    let send_wait_queue = msg_queue.send_wait_queue.clone();
    let should_cleanup = msg_queue.mark_removed && msg_queue.msqid_ds.msg_qnum == 0;
    drop(msg_queue);
    if should_remove {
        send_wait_queue.wake(usize::MAX, u32::MAX);
    }
    if should_cleanup {
        MSG_MANAGER.lock().remove_msqid(msqid);
    }

    Ok(copy_len as isize)
}

pub fn sys_msgctl(msqid: i32, cmd: i32, buf: usize) -> AxResult<isize> {
    //  Get current process information
    let current = current();
    let thread = current.as_thread();
    let cred = thread.cred();
    let current_uid = cred.euid;
    let current_gid = cred.egid;
    let is_privileged = current_uid == 0; // root user check
    let ns_id = thread.proc_data.nsproxy.lock().ipc_ns.lock().ns_id;

    // Validate command code
    if cmd != IPC_STAT
        && cmd != IPC_SET
        && cmd != IPC_RMID
        && cmd != IPC_INFO
        && cmd != MSG_INFO
        && cmd != MSG_STAT
    {
        // Simplified: do not support some Linux extensions
        return Err(AxError::from(LinuxError::EINVAL)); // EINVAL
    }

    // IPC_INFO (put before looking up the queue!)
    if cmd == IPC_INFO {
        // IPC_INFO uses msqid=0, no actual queue needed
        // Return system-level information
        #[repr(C)]
        struct MsgInfo {
            msgpool: i32,
            msgmap: i32,
            msgmax: i32,
            msgmnb: i32,
            msgmni: i32,
            msgssz: i32,
            msgtql: i32,
            msgseg: u16,
        }

        let info = MsgInfo {
            msgpool: 0,
            msgmap: 0,
            msgmax: MSGMAX as i32,
            msgmnb: MSGMNB as i32,
            msgmni: MSGMNI as i32,
            msgssz: 0,
            msgtql: 0,
            msgseg: 0,
        };

        // Copy to user space
        let ptr = buf as *mut MsgInfo;
        ptr.vm_write(info)?;
        return Ok(0);
    }

    // MSG_INFO (put before looking up the queue!)
    if cmd == MSG_INFO {
        let msg_manager = MSG_MANAGER.lock();
        let ns_queues_count = msg_manager
            .iter_active_queues()
            .filter(|(_, q)| q.lock().ns_id == ns_id)
            .count();
        let ns_total_bytes: usize = msg_manager
            .iter_active_queues()
            .filter(|(_, q)| q.lock().ns_id == ns_id)
            .map(|(_, q)| q.lock().total_bytes)
            .sum();

        let msg_perm = IpcPerm {
            key: 0,
            uid: current_uid,
            gid: current_gid,
            cuid: current_uid,
            cgid: current_gid,
            mode: 0o600,
            pad: 0,
            seq: 0,
            unused0: 0,
            unused1: 0,
        };

        let info_ds = msqid_ds {
            msg_perm,
            msg_stime: 0,
            msg_rtime: 0,
            msg_ctime: 0,
            msg_cbytes: ns_total_bytes as u64,
            msg_qnum: ns_queues_count as u64,
            msg_qbytes: MSGMNB as u64,
            msg_lspid: Pid::from(0u32) as _,
            msg_lrpid: Pid::from(0u32) as _,
        };

        let ptr = buf as *mut msqid_ds;
        ptr.vm_write(info_ds)?;

        return Ok(ns_queues_count as isize);
    }
    // MSG_STAT handling
    if cmd == MSG_STAT {
        let msg_manager = MSG_MANAGER.lock();

        let result = msg_manager
            .iter_active_queues()
            .filter(|(_, q)| q.lock().ns_id == ns_id)
            .nth(msqid as usize)
            .ok_or(AxError::from(LinuxError::EINVAL))
            .and_then(|(actual_msqid, queue)| {
                let guard = queue.lock();

                if !has_ipc_permission(&guard.msqid_ds.msg_perm, current_uid, current_gid, false) {
                    return Err(AxError::from(LinuxError::EACCES));
                }

                let ptr = buf as *mut msqid_ds;
                ptr.vm_write(guard.msqid_ds)?;
                Ok(actual_msqid as isize)
            });

        return result;
    }

    // Find message queue by msqid
    let msg_queue = {
        let msg_manager = MSG_MANAGER.lock();
        msg_manager
            .get_queue_by_msqid(msqid, ns_id)
            .ok_or(AxError::from(LinuxError::EINVAL))? // EINVAL - Queue does not exist
    };

    // Lock the internal structure of the queue
    let mut msg_queue = msg_queue.lock();
    // Check if the queue is marked as removed
    if msg_queue.mark_removed {
        return Err(AxError::from(LinuxError::EIDRM)); // EIDRM - Queue has been removed
    }
    if cmd == IPC_STAT {
        // Check read permissions
        if !has_ipc_permission(
            &msg_queue.msqid_ds.msg_perm,
            current_uid,
            current_gid,
            false,
        ) {
            return Err(AxError::from(LinuxError::EACCES)); // EACCES
        }

        // Copy queue status to user space
        let ptr = buf as *mut msqid_ds;
        ptr.vm_write(msg_queue.msqid_ds)?;

        return Ok(0);
    }

    // Check permissions (owner, creator, or privileged user)
    let is_owner = current_uid == msg_queue.msqid_ds.msg_perm.uid;
    let is_creator = current_uid == msg_queue.msqid_ds.msg_perm.cuid;

    if !is_privileged && !is_owner && !is_creator {
        return Err(AxError::from(LinuxError::EPERM)); // EPERM
    }

    if cmd == IPC_SET {
        // Read new settings from user space
        let ptr = buf as *const msqid_ds;
        let user_buf = ptr.vm_read()?;

        // Update permission information (fields allowed by man-page)
        msg_queue.msqid_ds.msg_perm.uid = user_buf.msg_perm.uid;
        msg_queue.msqid_ds.msg_perm.gid = user_buf.msg_perm.gid;
        msg_queue.msqid_ds.msg_perm.mode = user_buf.msg_perm.mode & 0o777; // Only take permission bits

        // Update queue size limit (requires privilege check)
        let old_qbytes = msg_queue.msqid_ds.msg_qbytes;
        if user_buf.msg_qbytes != old_qbytes {
            if user_buf.msg_qbytes > MSGMNB as u64 && !is_privileged {
                return Err(AxError::from(LinuxError::EPERM)); // EPERM - requires privilege to exceed MSGMNB
            }
            msg_queue.msqid_ds.msg_qbytes = user_buf.msg_qbytes;
        }

        // Update modification time
        msg_queue.msqid_ds.msg_ctime = monotonic_time_nanos() as _;

        if user_buf.msg_qbytes > old_qbytes {
            let send_wait_queue = msg_queue.send_wait_queue.clone();
            drop(msg_queue);
            send_wait_queue.wake(usize::MAX, u32::MAX);
        }

        return Ok(0);
    }
    if cmd == IPC_RMID {
        msg_queue.mark_removed = true;
        msg_queue.msqid_ds.msg_ctime = monotonic_time_nanos() as _;
        // Note: Linux keeps queued messages until drained; we clear immediately.
        msg_queue.messages.clear();
        msg_queue.total_bytes = 0;
        msg_queue.msqid_ds.msg_cbytes = 0;
        msg_queue.msqid_ds.msg_qnum = 0;

        let send_wait_queue = msg_queue.send_wait_queue.clone();
        let recv_wait_queue = msg_queue.recv_wait_queue.clone();
        drop(msg_queue);
        MSG_MANAGER.lock().remove_msqid(msqid);

        send_wait_queue.wake(usize::MAX, u32::MAX);
        recv_wait_queue.wake(usize::MAX, u32::MAX);

        return Ok(0);
    }
    // Currently unsupported operations
    // some Linux-specific extensions
    // These Linux-specific extensions are not implemented for now because the basic
    // operations are sufficient and these are not POSIX standard They can be
    // implemented later to support tools like ipcs
    Err(AxError::from(LinuxError::EINVAL)) // EINVAL
}
