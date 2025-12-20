use alloc::{collections::BTreeMap, sync::Arc, vec::Vec};

use axerrno::{AxError, AxResult, LinuxError};
use axhal::time::monotonic_time_nanos;
use axsync::Mutex;
use axtask::current;
use bytemuck::AnyBitPattern;
use linux_raw_sys::general::*;
use starry_core::{shm::IpcPerm, task::AsThread};
use starry_process::Pid;
use starry_vm::{VmMutPtr, VmPtr, vm_load, vm_write_slice};

use super::{
    IPC_CREAT, IPC_EXCL, IPC_INFO, IPC_PRIVATE, IPC_RMID, IPC_SET, IPC_STAT, MSG_INFO, MSG_STAT,
    has_ipc_permission, next_ipc_id,
};
use crate::syscall::{sys_getgid, sys_getuid};

/// Data structure describing a message queue.
#[repr(C)]
#[derive(Clone, Copy, AnyBitPattern)]
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
}

/// This struct is used to maintain the message queue in kernel.
pub struct MessageQueue {
    /// Message queue data structure
    pub msqid_ds: msqid_ds,
    /// Queue of messages
    pub messages: BTreeMap<i64, Vec<Message>>, // mtype -> messages of that type
    /// Total bytes in queue
    pub total_bytes: usize,
    /// Marked for removal
    pub mark_removed: bool,
}

impl MessageQueue {
    /// Creates a new [`MessageQueue`].
    pub fn new(key: i32, mode: __kernel_mode_t, pid: Pid, uid: u32, gid: u32) -> Self {
        MessageQueue {
            msqid_ds: msqid_ds::new(key, mode, pid as __kernel_pid_t, uid, gid),
            messages: BTreeMap::new(),
            total_bytes: 0,
            mark_removed: false,
        }
    }

    /// Add a message to the queue
    pub fn enqueue_message(&mut self, mtype: i64, data: Vec<u8>) -> AxResult<()> {
        let data_len = data.len();
        // Check queue size limits
        if self.total_bytes + data_len > self.msqid_ds.msg_qbytes as usize {
            return Err(AxError::from(LinuxError::ENOSPC)); // ENOSPC
        }

        let message = Message { mtype, data };

        self.messages.entry(mtype).or_default().push(message);
        self.total_bytes += data_len;
        self.msqid_ds.msg_cbytes += data_len as __kernel_size_t;
        self.msqid_ds.msg_qnum += 1;

        Ok(())
    }

    /// Find the first message (without removing)
    pub fn find_first_message(&self) -> Option<(i64, &[u8])> {
        for (&mtype, messages) in &self.messages {
            if let Some(message) = messages.first() {
                return Some((mtype, &message.data[..]));
            }
        }
        None
    }

    /// Find message by type (without removing)
    pub fn find_message_by_type(&self, msgtyp: i64) -> Option<(i64, &[u8])> {
        self.messages
            .get(&msgtyp)
            .and_then(|msgs| msgs.first())
            .map(|msg| (msgtyp, &msg.data[..]))
    }

    /// Find the first message with a type not equal to the specified value
    /// (without removing)
    pub fn find_message_not_equal(&self, msgtyp: i64) -> Option<(i64, &[u8])> {
        for (&mtype, messages) in &self.messages {
            if mtype != msgtyp
                && let Some(message) = messages.first()
            {
                return Some((mtype, &message.data[..]));
            }
        }
        None
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

    /// Get message by index (for MSG_COPY)
    pub fn get_message_by_index(&self, index: usize) -> Option<&Message> {
        let mut current_index = 0;

        // Iterate over all messages in order of message type
        for messages in self.messages.values() {
            if index < current_index + messages.len() {
                return messages.get(index - current_index);
            }
            current_index += messages.len();
        }
        None
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

/// Message queue manager
pub struct MsgManager {
    /// key -> msqid mapping
    key_msqid: BTreeMap<i32, i32>,
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
    pub fn get_msqid_by_key(&self, key: i32) -> Option<i32> {
        self.key_msqid.get(&key).cloned()
    }

    /// Returns the message queue associated with the given ID.
    pub fn get_queue_by_msqid(&self, msqid: i32) -> Option<Arc<Mutex<MessageQueue>>> {
        self.msqid_queues.get(&msqid).cloned()
    }

    /// Inserts a mapping from a key to a message queue ID.
    pub fn insert_key_msqid(&mut self, key: i32, msqid: i32) {
        self.key_msqid.insert(key, msqid);
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

    /// get total bytes in all queues
    pub fn total_bytes(&self) -> usize {
        self.iter_active_queues()
            .map(|(_, queue)| {
                let guard = queue.lock();
                guard.total_bytes
            })
            .sum()
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
        const MSG_COPY = 0o20000;
        /// Receive any message except of specified type (Linux extension)
        const MSG_EXCEPT = 0o2000;
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
    let current_uid = sys_getuid()? as u32;
    let current_gid = sys_getgid()? as u32;
    let current_pid = proc_data.proc.pid();

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
        )));

        msg_manager.insert_msqid_queues(msqid, msg_queue);
        return Ok(msqid as isize);
    }

    // Look for existing message queue
    if let Some(msqid) = msg_manager.get_msqid_by_key(key) {
        let msg_queue = msg_manager
            .get_queue_by_msqid(msqid)
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
    )));

    msg_manager.insert_key_msqid(key, msqid);
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
    let current_uid = sys_getuid()? as u32;
    let current_gid = sys_getgid()? as u32;
    let current_pid = proc_data.proc.pid();
    let flags = MsgSndFlags::from_bits_truncate(msgflg);

    let msg_queue = {
        let msg_manager = MSG_MANAGER.lock();
        msg_manager
            .get_queue_by_msqid(msqid)
            .ok_or(AxError::from(LinuxError::EINVAL))? // EINVAL - queue does not exist
    };

    let mut msg_queue = msg_queue.lock();

    if !has_ipc_permission(
        &msg_queue.msqid_ds.msg_perm,
        current_uid as _,
        current_gid as _,
        true,
    ) {
        return Err(AxError::from(LinuxError::EACCES)); // EACCES
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

    // check if the message queue is marked for removal
    // Note: According to Linux manpage, both byte count and message count
    // are limited by msg_qbytes field (this appears to be the actual behavior)
    let would_exceed_bytes =
        msg_queue.total_bytes + data_vec.len() > msg_queue.msqid_ds.msg_qbytes as usize;
    let would_exceed_messages =
        (msg_queue.msqid_ds.msg_qnum + 1) as usize > msg_queue.msqid_ds.msg_qbytes as usize;

    if would_exceed_bytes || would_exceed_messages {
        // If the non-blocking flag is specified, return an error immediately
        if flags.contains(MsgSndFlags::IPC_NOWAIT) {
            return Err(AxError::from(LinuxError::EAGAIN)); // EAGAIN
        }

        // TODO:
        warn!("sys_msgsnd: blocking send not implemented, returning EAGAIN");
        // Otherwise, block and wait (blocking logic needs to be implemented
        // here) In the actual implementation, this should:
        // - Add the current task to the wait queue
        // - Yield the CPU and wait to be woken up when there is space in the
        //   queue
        // - After being woken up, recheck the condition
        // Note: It may be interrupted by a signal returning EINTR, or the queue
        // may be deleted returning EIDRM

        return Err(AxError::from(LinuxError::EAGAIN)); // EAGAIN
    }

    msg_queue.enqueue_message(mtype, data_vec)?;

    msg_queue.msqid_ds.msg_lspid = current_pid as _;

    msg_queue.msqid_ds.msg_stime = monotonic_time_nanos() as _;

    // note:msg_qnum and msg_cbytes updated in enqueue_message

    // TODO:
    warn!("sys_msgsnd: wakeup of waiting receivers not implemented");
    // If there are processes waiting to receive messages, wake them up
    // In the actual implementation, this should:
    // - Check if there are tasks in the message queue's wait queue
    // - If so, wake up these tasks
    Ok(0)
}

pub fn sys_msgrcv(
    msqid: i32,
    msgp: *mut UserMsgbuf,
    msgsz: usize,
    msgtyp: i64,
    msgflg: i32,
) -> AxResult<isize> {
    // Parse flags and get current process information

    let flags = MsgRcvFlags::from_bits_truncate(msgflg);
    let current = current();
    let thread = current.as_thread();
    let proc_data = &thread.proc_data;
    let current_uid = sys_getuid()? as u32;
    let current_gid = sys_getgid()? as u32;
    let current_pid = proc_data.proc.pid();

    // Check validity of flag combinations
    if flags.contains(MsgRcvFlags::MSG_COPY) {
        if !flags.contains(MsgRcvFlags::IPC_NOWAIT) {
            return Err(AxError::from(LinuxError::EINVAL)); // EINVAL - MSG_COPY must be used with IPC_NOWAIT
        }
        if flags.contains(MsgRcvFlags::MSG_EXCEPT) {
            return Err(AxError::from(LinuxError::EINVAL)); // EINVAL - MSG_COPY and MSG_EXCEPT are mutually exclusive
        }
    }

    // Get the message queue
    let msg_queue = {
        let msg_manager = MSG_MANAGER.lock();
        msg_manager
            .get_queue_by_msqid(msqid)
            .ok_or(AxError::from(LinuxError::EINVAL))? // EINVAL
    };

    let mut msg_queue = msg_queue.lock();

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
    let (mtype, data_slice, index, should_remove) = if flags.contains(MsgRcvFlags::MSG_COPY) {
        // MSG_COPY mode: msgtyp is the message index
        let index = msgtyp as usize;

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
        // Normal mode: msgtyp is the message type
        let matched_message = match msgtyp {
            0 => msg_queue.find_first_message(), // First message
            typ if typ > 0 => {
                if flags.contains(MsgRcvFlags::MSG_EXCEPT) {
                    msg_queue.find_message_not_equal(typ) // Type not equal to msgtyp
                } else {
                    msg_queue.find_message_by_type(typ) // Type equal to msgtyp
                }
            }
            typ if typ < 0 => {
                let abs_typ = typ.abs();
                msg_queue.find_message_less_equal(abs_typ) // Type ≤ |msgtyp|
            }
            _ => None,
        };

        // Index is always 0 in normal mode
        let index = 0;

        // Handle no message situation
        let (mtype, data_slice) = match matched_message {
            Some((mtype, data_slice)) => (mtype, data_slice),
            None => {
                if flags.contains(MsgRcvFlags::IPC_NOWAIT) {
                    return Err(AxError::from(LinuxError::ENOMSG)); // ENOMSG
                }

                // TODO:
                warn!("sys_msgrcv: blocking receive not implemented, returning ENOMSG");
                // The complete implementation should:
                // - Add the current task to the receive wait queue
                // - Block and wait, possibly interrupted by signals (EINTR) or queue removal
                //   (EIDRM)
                // Simplified: blocking is not supported, directly return an error
                return Err(AxError::from(LinuxError::ENOMSG)); // ENOMSG
            }
        };

        (mtype, data_slice, index, true) // should_remove = true
    };

    // Message size check
    if data_slice.len() > msgsz {
        if flags.contains(MsgRcvFlags::MSG_NOERROR) {
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

        // TODO:
        warn!("sys_msgrcv: wakeup of waiting senders not implemented");
        // Wake up waiting senders (Simplified: not implemented)
        // while let Some(task) = msg_queue.send_wait_queue.pop_front() {
        //     wakeup(task);
        // }
    } else {
        // MSG_COPY mode: only update last receiver info, do not update queue statistics
        msg_queue.msqid_ds.msg_lrpid = current_pid as _;
        msg_queue.msqid_ds.msg_rtime = monotonic_time_nanos() as _;
    }

    Ok(copy_len as isize)
}

pub fn sys_msgctl(msqid: i32, cmd: i32, buf: usize) -> AxResult<isize> {
    //  Get current process information
    let current_uid = sys_getuid()? as u32;
    let current_gid = sys_getgid()? as u32;
    let is_privileged = current_uid == 0; // root user check

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
        // Manually create IpcPerm
        let msg_perm = starry_core::shm::IpcPerm {
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

        // Create a temporary msqid_ds to return information
        let info_ds = msqid_ds {
            msg_perm,
            msg_stime: 0,
            msg_rtime: 0,
            msg_ctime: 0,
            msg_cbytes: msg_manager.total_bytes() as u64,
            // Use msg_qnum to return the number of allocated queues
            msg_qnum: msg_manager.queue_count() as u64,
            // Use msg_qbytes to return system limits or usage
            msg_qbytes: MSGMNB as u64,
            msg_lspid: Pid::from(0u32) as _,
            msg_lrpid: Pid::from(0u32) as _,
        };

        // Copy to user space
        let ptr = buf as *mut msqid_ds;
        ptr.vm_write(info_ds)?;

        // Return the current number of allocated queues
        return Ok(msg_manager.queue_count() as isize);
    }
    // MSG_STAT handling
    if cmd == MSG_STAT {
        let msg_manager = MSG_MANAGER.lock();

        let result = msg_manager
            .iter_active_queues()
            .nth(msqid as usize)
            .ok_or(AxError::from(LinuxError::EINVAL))
            .and_then(|(actual_msqid, queue)| {
                let guard = queue.lock();

                if !has_ipc_permission(
                    &guard.msqid_ds.msg_perm,
                    current_uid,
                    current_gid,
                    false, // read permission check
                ) {
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
            .get_queue_by_msqid(msqid)
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
        if user_buf.msg_qbytes != msg_queue.msqid_ds.msg_qbytes {
            if user_buf.msg_qbytes > MSGMNB as _ && !is_privileged {
                return Err(AxError::from(LinuxError::EPERM)); // EPERM - requires privilege to exceed MSGMNB
            }
            msg_queue.msqid_ds.msg_qbytes = user_buf.msg_qbytes;
        }

        // Update modification time
        msg_queue.msqid_ds.msg_ctime = monotonic_time_nanos() as _;

        return Ok(0);
    }
    if cmd == IPC_RMID {
        // Mark the queue as removed
        msg_queue.mark_removed = true;

        // If the queue is empty, delete it immediately
        if msg_queue.msqid_ds.msg_qnum == 0 {
            drop(msg_queue); // Release the lock to avoid deadlock

            MSG_MANAGER.lock().remove_msqid(msqid);

            // TODO:
            warn!(
                "sys_msgctl[IPC_RMID]: wakeup of waiting processes after queue deletion not \
                 implemented"
            );
            // Wake up all waiting processes (simplified: not implemented yet)
            // According to man-page: wake up all waiting readers and writers (returning
            // EIDRM error)

            return Ok(0);
        }

        // If the queue is not empty, only mark it as removed and wait for all messages
        // to be taken before automatic deletion Update modification time
        msg_queue.msqid_ds.msg_ctime = monotonic_time_nanos() as _;

        return Ok(0);
    }
    // Currently unsupported operations
    // some Linux-specific extensions
    // These Linux-specific extensions are not implemented for now because the basic
    // operations are sufficient and these are not POSIX standard They can be
    // implemented later to support tools like ipcs
    Err(AxError::from(LinuxError::EINVAL)) // EINVAL
}
