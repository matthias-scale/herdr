use super::*;
use std::ffi::CStr;
use std::os::{fd::AsRawFd, unix::fs::MetadataExt};

fn try_set_attribute(file: &std::fs::File, name: &CStr, value: &[u8]) -> std::io::Result<()> {
    let result = unsafe {
        libc::fsetxattr(
            file.as_raw_fd(),
            name.as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            0,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

fn set_attribute(file: &std::fs::File, name: &CStr, value: &[u8]) {
    try_set_attribute(file, name, value).unwrap();
}

fn attribute(file: &std::fs::File, name: &CStr) -> Option<Vec<u8>> {
    let mut value = vec![0; 1024];
    let read = unsafe {
        libc::fgetxattr(
            file.as_raw_fd(),
            name.as_ptr(),
            value.as_mut_ptr().cast(),
            value.len(),
        )
    };
    if read < 0 {
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ENODATA)
        );
        return None;
    }
    value.truncate(read as usize);
    Some(value)
}

#[test]
fn config_metadata_preserves_ownership_and_acl_without_inheriting_extra_access() {
    let dir = std::env::temp_dir().join(format!("herdr-config-acl-{}", std::process::id()));
    std::fs::create_dir(&dir).unwrap();
    // Linux UAPI posix_acl_xattr_header/entry, version 2, little-endian fields.
    // Owner rw, named user 65534 read, group none, mask read, other none.
    let mut acl = 2_u32.to_le_bytes().to_vec();
    for (tag, permissions, id) in [
        (1_u16, 6_u16, u32::MAX),
        (2, 4, 65534),
        (4, 0, u32::MAX),
        (16, 4, u32::MAX),
        (32, 0, u32::MAX),
    ] {
        acl.extend(tag.to_le_bytes());
        acl.extend(permissions.to_le_bytes());
        acl.extend(id.to_le_bytes());
    }
    let probe_path = dir.join("acl-probe");
    std::fs::write(&probe_path, b"probe").unwrap();
    let probe = std::fs::File::open(&probe_path).unwrap();
    let acl_supported = match try_set_attribute(&probe, c"system.posix_acl_access", &acl) {
        Ok(()) => true,
        Err(error)
            if matches!(
                error.raw_os_error(),
                Some(code)
                    if [
                        libc::EACCES,
                        libc::EINVAL,
                        libc::ENOSYS,
                        libc::EOPNOTSUPP,
                        libc::EPERM,
                    ]
                    .contains(&code)
            ) =>
        {
            false
        }
        Err(error) => panic!("ACL capability probe failed: {error}"),
    };
    drop(probe);
    std::fs::remove_file(probe_path).unwrap();
    for has_acl in [false, true] {
        if has_acl && !acl_supported {
            continue;
        }
        let source = dir.join(format!("source-{has_acl}"));
        let target = dir.join(format!("target-{has_acl}"));
        std::fs::write(&source, b"old").unwrap();
        let input = std::fs::File::open(&source).unwrap();
        if unsafe { libc::geteuid() } == 0 {
            assert_eq!(unsafe { libc::fchown(input.as_raw_fd(), 1001, 1002) }, 0);
        }
        if has_acl {
            set_attribute(&input, c"system.posix_acl_access", &acl);
        }
        set_attribute(&input, c"user.herdr-test", b"preserve this attribute");
        let original = input.metadata().unwrap();
        drop(create_config_temporary(&target, true).unwrap());
        let output = std::fs::File::open(&target).unwrap();
        // Model a default ACL inherited from the destination's parent directory.
        if acl_supported {
            set_attribute(&output, c"system.posix_acl_access", &acl);
        }
        write_config_temporary(Some(&source), &target, b"new").unwrap();
        let actual = output.metadata().unwrap();
        assert_eq!(
            (actual.uid(), actual.gid(), actual.mode()),
            (original.uid(), original.gid(), original.mode())
        );
        assert_eq!(
            attribute(&output, c"system.posix_acl_access"),
            attribute(&input, c"system.posix_acl_access")
        );
        assert_eq!(
            attribute(&output, c"user.herdr-test"),
            Some(b"preserve this attribute".to_vec())
        );
        assert_eq!(std::fs::read(source).unwrap(), b"old");
        assert_eq!(std::fs::read(target).unwrap(), b"new");
    }
    std::fs::remove_dir_all(dir).unwrap();
}
