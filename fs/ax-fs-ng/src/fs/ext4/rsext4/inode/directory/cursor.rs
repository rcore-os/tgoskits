//! Linux directory cookies and private ext4 continuation state.

use axfs_ng_vfs::{DirectoryCursor as VfsDirectoryCursor, VfsError, VfsResult};
use rsext4::DirectoryCursor;

// Linux reserves this value for HTree EOF. ext4's directory hash finalization
// rewrites the only colliding major hash, 0xffff_fffe, to 0xffff_fffc.
const HTREE_EOF_COOKIE: u64 = i64::MAX as u64;

pub(super) fn vfs_to_core_directory_cursor(
    cursor: VfsDirectoryCursor,
    indexed: bool,
) -> VfsResult<DirectoryCursor> {
    if cursor.offset() == HTREE_EOF_COOKIE {
        return Ok(DirectoryCursor::End);
    }
    if cursor.offset() == 0 && cursor.continuation() == 0 {
        return Ok(DirectoryCursor::Start);
    }
    if !indexed {
        return Ok(DirectoryCursor::Linear {
            offset: cursor.offset(),
        });
    }
    let collision = u32::try_from(cursor.continuation()).map_err(|_| VfsError::InvalidInput)?;
    Ok(DirectoryCursor::HTree {
        major: ((cursor.offset() >> 32) as u32) << 1,
        minor: cursor.offset() as u32,
        collision,
    })
}

pub(super) fn core_to_vfs_directory_cursor(
    cursor: DirectoryCursor,
    change_attribute: Option<u64>,
) -> VfsDirectoryCursor {
    let (offset, continuation) = match cursor {
        DirectoryCursor::Start => (0, 0),
        DirectoryCursor::Linear { offset } => (offset, 0),
        DirectoryCursor::HTree {
            major,
            minor,
            collision,
        } => (
            (u64::from(major >> 1) << 32) | u64::from(minor),
            u64::from(collision),
        ),
        DirectoryCursor::End => (HTREE_EOF_COOKIE, 0),
    };
    match change_attribute {
        Some(change_attribute) => VfsDirectoryCursor::with_observed_change_attribute(
            offset,
            continuation,
            change_attribute,
        ),
        None => VfsDirectoryCursor::with_continuation(offset, continuation),
    }
}

pub(super) fn normalize_directory_cursor(
    cursor: VfsDirectoryCursor,
    change_attribute: u64,
) -> VfsDirectoryCursor {
    let continuation = match cursor.observed_change_attribute() {
        Some(observed) if observed != change_attribute => 0,
        _ => cursor.continuation(),
    };
    VfsDirectoryCursor::with_observed_change_attribute(
        cursor.offset(),
        continuation,
        change_attribute,
    )
}

#[cfg(test)]
mod directory_cursor_tests {
    use super::*;

    #[test]
    fn linux_64_bit_htree_cookie_round_trips_private_collision_state() {
        let core = DirectoryCursor::HTree {
            major: 0x89ab_cdec,
            minor: 0x1357_2468,
            collision: 7,
        };
        let vfs = core_to_vfs_directory_cursor(core, Some(41));

        assert_eq!(vfs.offset(), 0x44d5_e6f6_1357_2468);
        assert_eq!(vfs.continuation(), 7);
        assert_eq!(vfs.observed_change_attribute(), Some(41));
        assert_eq!(vfs_to_core_directory_cursor(vfs, true), Ok(core));
    }

    #[test]
    fn external_seek_cookie_resets_private_collision_state() {
        let cookie = core_to_vfs_directory_cursor(
            DirectoryCursor::HTree {
                major: 0x1234_5678,
                minor: 0x9abc_def0,
                collision: 11,
            },
            Some(7),
        );
        let external_seek = VfsDirectoryCursor::new(cookie.offset());

        assert_eq!(
            vfs_to_core_directory_cursor(external_seek, true),
            Ok(DirectoryCursor::HTree {
                major: 0x1234_5678,
                minor: 0x9abc_def0,
                collision: 0,
            })
        );
    }

    #[test]
    fn htree_eof_cookie_maps_to_core_end() {
        let cursor = VfsDirectoryCursor::new(HTREE_EOF_COOKIE);
        assert_eq!(
            vfs_to_core_directory_cursor(cursor, true),
            Ok(DirectoryCursor::End)
        );
        assert_eq!(
            core_to_vfs_directory_cursor(DirectoryCursor::End, None),
            cursor
        );
    }

    #[test]
    fn largest_linux_directory_hash_does_not_collide_with_eof() {
        let core = DirectoryCursor::HTree {
            major: 0xffff_fffc,
            minor: u32::MAX,
            collision: 0,
        };
        let vfs = core_to_vfs_directory_cursor(core, None);

        assert_eq!(vfs.offset(), 0x7fff_fffe_ffff_ffff);
        assert_ne!(vfs.offset(), HTREE_EOF_COOKIE);
        assert_eq!(vfs_to_core_directory_cursor(vfs, true), Ok(core));
    }

    #[test]
    fn directory_mutation_discards_private_collision_continuation() {
        let stale =
            VfsDirectoryCursor::with_observed_change_attribute(0x1234_5678_9abc_def0, 11, 41);

        let current = normalize_directory_cursor(stale, 42);

        assert_eq!(current.offset(), stale.offset());
        assert_eq!(current.continuation(), 0);
        assert_eq!(current.observed_change_attribute(), Some(42));
    }

    #[test]
    fn unchanged_directory_keeps_private_collision_continuation() {
        let cursor =
            VfsDirectoryCursor::with_observed_change_attribute(0x1234_5678_9abc_def0, 11, 42);

        assert_eq!(normalize_directory_cursor(cursor, 42), cursor);
    }
}
