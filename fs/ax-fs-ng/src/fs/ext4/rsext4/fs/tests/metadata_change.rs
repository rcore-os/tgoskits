//! Native metadata commands on real mounted inodes, not normalized owned patches.

use core::time::Duration;

use axfs_ng_vfs::{
    Location, Metadata, MetadataChange, MutationCredentials, NodePermission, NodeType, TimeChange,
    VfsError,
};

use super::{Ext4Filesystem, InodeNumber};

#[test]
fn metadata_commands_share_current_inode_authority_through_aliases_and_unlink() {
    let (filesystem, root, _) = super::sync_policy::background_mount();
    let privileged = MutationCredentials::root();
    let owner = MutationCredentials {
        fsuid: 1000,
        fsgid: 1000,
        supplementary_gids: &[2000],
        cap_dac_override: false,
        cap_dac_read_search: false,
        cap_fowner: false,
        cap_mknod: false,
        cap_chown: false,
        cap_fsetid: false,
    };
    let successor = MutationCredentials {
        fsuid: 2000,
        fsgid: 3000,
        supplementary_gids: &[],
        cap_dac_override: false,
        cap_dac_read_search: false,
        cap_fowner: false,
        cap_mknod: false,
        cap_chown: false,
        cap_fsetid: false,
    };
    let stranger = MutationCredentials {
        fsuid: 4000,
        fsgid: 4000,
        supplementary_gids: &[],
        cap_dac_override: false,
        cap_dac_read_search: false,
        cap_fowner: false,
        cap_mknod: false,
        cap_chown: false,
        cap_fsetid: false,
    };
    let directory = root
        .create(
            "commands",
            NodeType::Directory,
            mode(0o777),
            0,
            0,
            &privileged,
        )
        .unwrap();
    // Legacy UID/GID arguments do not grant authority or select the owner.
    let input = directory
        .create("input", NodeType::RegularFile, mode(0o600), 0, 0, &owner)
        .unwrap();
    let alias = directory.link("alias", &input, &owner).unwrap();
    input.entry().as_file().unwrap().set_len(7).unwrap();
    let initial = input.metadata().unwrap();
    assert_eq!((initial.uid, initial.gid), (1000, 1000));
    assert_eq!(alias.inode(), input.inode());

    // First behavior oracle: the real owner must be able to chmod its inode.
    // Unmigrated Ext4 returns OperationNotSupported here, not a fixture error.
    assert_eq!(
        alias.change_metadata(MetadataChange::Chmod(mode(0o6750)), &owner),
        Ok(()),
        "native Ext4 must execute an authorized metadata command"
    );
    assert_aliases(&input, &alias, (1000, 1000, 0o6750));
    assert_times(&input.metadata().unwrap(), initial.atime, initial.mtime);
    for command in [
        MetadataChange::Chown {
            uid: Some(2000),
            gid: None,
        },
        MetadataChange::Chown {
            uid: None,
            gid: Some(3000),
        },
    ] {
        reject_unchanged(&alias, command, &owner);
    }

    // Partial group changes preserve UID; executable SGID and SUID are killed.
    change(
        &input,
        MetadataChange::Chown {
            uid: None,
            gid: Some(2000),
        },
        &owner,
    );
    assert_aliases(&input, &alias, (1000, 2000, 0o750));
    change(&alias, MetadataChange::Chmod(mode(0o6640)), &owner);
    change(
        &input,
        MetadataChange::Chown {
            uid: Some(1000),
            gid: None,
        },
        &owner,
    );
    // Non-executable SGID survives for a caller in the current group.
    assert_aliases(&input, &alias, (1000, 2000, 0o2640));
    change(
        &alias,
        MetadataChange::Chown {
            uid: None,
            gid: None,
        },
        &owner,
    );
    assert_aliases(&input, &alias, (1000, 2000, 0o2640));

    // Linux chown_common never exempts SUID merely because CAP_FSETID is set.
    change(&input, MetadataChange::Chmod(mode(0o6750)), &privileged);
    change(
        &alias,
        MetadataChange::Chown {
            uid: Some(2000),
            gid: None,
        },
        &privileged,
    );
    assert_aliases(&input, &alias, (2000, 2000, 0o750));
    // Some(current_value) still checks the *current* owner after transfer.
    for command in [
        MetadataChange::Chmod(mode(0o600)),
        MetadataChange::Chown {
            uid: Some(2000),
            gid: None,
        },
        MetadataChange::Chown {
            uid: None,
            gid: Some(2000),
        },
    ] {
        reject_unchanged(&input, command, &owner);
    }
    // The new owner can retain the current group even without membership.
    change(
        &input,
        MetadataChange::Chown {
            uid: None,
            gid: Some(2000),
        },
        &successor,
    );
    assert_aliases(&input, &alias, (2000, 2000, 0o750));
    change(&alias, MetadataChange::Chmod(mode(0o6750)), &successor);
    // SGID uses the new owner's current group membership, not the old caller's.
    assert_aliases(&input, &alias, (2000, 2000, 0o4750));
    change(&input, MetadataChange::Chmod(mode(0o2640)), &privileged);
    let preserve_setgid = MutationCredentials {
        cap_fsetid: true,
        ..stranger
    };
    // No fields and no derived MODE: this is nonempty, but not owner-only.
    change(
        &alias,
        MetadataChange::Chown {
            uid: None,
            gid: None,
        },
        &preserve_setgid,
    );
    assert_aliases(&input, &alias, (2000, 2000, 0o2640));
    // Without CAP_FSETID, killing this SGID derives MODE and requires ownership.
    reject_unchanged(
        &alias,
        MetadataChange::Chown {
            uid: None,
            gid: None,
        },
        &stranger,
    );
    assert_times(&input.metadata().unwrap(), initial.atime, initial.mtime);

    let inheriting = directory
        .create("inheriting", NodeType::Directory, mode(0o770), 0, 0, &owner)
        .unwrap();
    // Establish SGID explicitly: this tests chown, not mkdir's mode policy.
    change(&inheriting, MetadataChange::Chmod(mode(0o2770)), &owner);
    change(
        &inheriting,
        MetadataChange::Chown {
            uid: None,
            gid: Some(2000),
        },
        &owner,
    );
    let metadata = inheriting.metadata().unwrap();
    assert_eq!(
        (metadata.uid, metadata.gid, metadata.mode.bits()),
        (1000, 2000, 0o2770)
    );

    change(&input, MetadataChange::Chmod(mode(0o666)), &successor);
    exercise_times(&input, &alias, &successor, &stranger);
    filesystem.sync_to_disk().unwrap();
    let looked_up = directory.lookup_no_follow("alias").unwrap();
    assert_same_metadata(&looked_up.metadata().unwrap(), &input.metadata().unwrap());
    assert_canonical(&filesystem, &alias);

    directory.unlink("input", false, &privileged).unwrap();
    directory.unlink("alias", false, &privileged).unwrap();
    let unlinked = input.metadata().unwrap();
    assert_eq!(unlinked.nlink, 0);
    // An open zero-link inode remains a valid metadata target, not a parent.
    change(&alias, MetadataChange::Chmod(mode(0o640)), &successor);
    change(
        &input,
        MetadataChange::Chown {
            uid: None,
            gid: Some(3000),
        },
        &successor,
    );
    assert_aliases(&input, &alias, (2000, 3000, 0o640));
    assert_times(&alias.metadata().unwrap(), unlinked.atime, unlinked.mtime);
    filesystem.sync_to_disk().unwrap();
    assert_canonical(&filesystem, &alias);
    assert_eq!(input.metadata().unwrap().nlink, 0);
}

fn exercise_times(
    input: &Location,
    alias: &Location,
    owner: &MutationCredentials<'_>,
    writer: &MutationCredentials<'_>,
) {
    use TimeChange::{Keep, Now, Set};

    change(
        input,
        MetadataChange::Times {
            atime: Set(Duration::from_secs(11)),
            mtime: Set(Duration::from_secs(22)),
        },
        owner,
    );
    assert_times(
        &alias.metadata().unwrap(),
        Duration::from_secs(11),
        Duration::from_secs(22),
    );
    // Ordinary write access authorizes double-Now, not explicit time changes.
    for command in [
        MetadataChange::Times {
            atime: Keep,
            mtime: Now,
        },
        MetadataChange::Times {
            atime: Set(Duration::from_secs(33)),
            mtime: Keep,
        },
    ] {
        reject_unchanged(alias, command, writer);
    }
    let before = input.metadata().unwrap();
    change(
        alias,
        MetadataChange::Times {
            atime: Keep,
            mtime: Keep,
        },
        writer,
    );
    assert_same_metadata(&input.metadata().unwrap(), &before);
    change(
        alias,
        MetadataChange::Times {
            atime: Now,
            mtime: Now,
        },
        writer,
    );
    let touched = input.metadata().unwrap();
    // AX currently exposes only seconds. This proves common sampling, not
    // freshness: this fixture has no controlled wall-clock provider.
    assert_times(&touched, touched.ctime, touched.ctime);
    assert_eq!(touched.ctime.subsec_nanos(), 0);
    change(
        input,
        MetadataChange::Times {
            atime: Set(Duration::from_secs(31)),
            mtime: Keep,
        },
        owner,
    );
    assert_times(
        &alias.metadata().unwrap(),
        Duration::from_secs(31),
        touched.mtime,
    );
    change(
        alias,
        MetadataChange::Times {
            atime: Keep,
            mtime: Set(Duration::from_secs(47)),
        },
        owner,
    );
    assert_times(
        &input.metadata().unwrap(),
        Duration::from_secs(31),
        Duration::from_secs(47),
    );
    change(
        input,
        MetadataChange::Times {
            atime: Keep,
            mtime: Now,
        },
        owner,
    );
    let explicit = alias.metadata().unwrap();
    assert_times(&explicit, Duration::from_secs(31), explicit.ctime);
    assert_aliases(input, alias, (2000, 2000, 0o666));
}

fn mode(bits: u16) -> NodePermission {
    NodePermission::from_bits_truncate(bits)
}

fn change(location: &Location, command: MetadataChange, credentials: &MutationCredentials<'_>) {
    assert_eq!(
        location.change_metadata(command, credentials),
        Ok(()),
        "{command:?}"
    );
}

fn reject_unchanged(
    location: &Location,
    command: MetadataChange,
    credentials: &MutationCredentials<'_>,
) {
    let before = location.metadata().unwrap();
    assert_eq!(
        location.change_metadata(command, credentials),
        Err(VfsError::OperationNotPermitted),
        "{command:?}"
    );
    assert_same_metadata(&location.metadata().unwrap(), &before);
}

fn assert_aliases(input: &Location, alias: &Location, expected: (u32, u32, u16)) {
    let metadata = input.metadata().unwrap();
    assert_eq!((metadata.uid, metadata.gid, metadata.mode.bits()), expected);
    assert_eq!(metadata.size, 7);
    assert_same_metadata(&alias.metadata().unwrap(), &metadata);
}

fn assert_times(metadata: &Metadata, atime: Duration, mtime: Duration) {
    assert_eq!((metadata.atime, metadata.mtime), (atime, mtime));
}

fn assert_same_metadata(actual: &Metadata, expected: &Metadata) {
    assert_eq!(
        (
            actual.inode,
            actual.uid,
            actual.gid,
            actual.mode.bits(),
            actual.nlink,
            actual.size
        ),
        (
            expected.inode,
            expected.uid,
            expected.gid,
            expected.mode.bits(),
            expected.nlink,
            expected.size
        )
    );
    assert_times(actual, expected.atime, expected.mtime);
    assert_eq!(actual.ctime, expected.ctime);
    assert_eq!(
        (
            actual.device,
            actual.node_type,
            actual.blocks,
            actual.block_size,
            actual.rdev
        ),
        (
            expected.device,
            expected.node_type,
            expected.blocks,
            expected.block_size,
            expected.rdev
        )
    );
}

fn assert_canonical(filesystem: &Ext4Filesystem, location: &Location) {
    let metadata = location.metadata().unwrap();
    let number = InodeNumber::new(location.inode() as u32).unwrap();
    let mut state = filesystem.lock();
    let inode = state.ext4.inode(number).unwrap();
    assert_eq!(
        (inode.uid, inode.gid, inode.mode & 0o7777),
        (metadata.uid, metadata.gid, metadata.mode.bits())
    );
    assert_eq!(
        (u64::from(inode.links), inode.size),
        (metadata.nlink, metadata.size)
    );
    assert_eq!(u64::from(inode.atime), metadata.atime.as_secs());
    assert_eq!(u64::from(inode.mtime), metadata.mtime.as_secs());
    assert_eq!(u64::from(inode.ctime), metadata.ctime.as_secs());
}
