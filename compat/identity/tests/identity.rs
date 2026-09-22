use codex_package_identity::{write_string, Identity};
use windows_sys::Win32::Storage::Packaging::Appx::PACKAGE_ID;
const XML: &str = include_str!("../../test-manifest.xml");

#[test]
fn derives_names_and_obeys_win32_buffer_contract() {
    let identity = Identity::parse(XML).unwrap();
    assert_eq!(
        String::from_utf16_lossy(&identity.full),
        "OpenAI.Codex_26.915.4065.0_x64__2p2nqsd0c76g0\0"
    );
    unsafe {
        assert_eq!(
            identity.write_id(std::ptr::null_mut(), std::ptr::null_mut()),
            87
        );
        let mut len = 0;
        assert_eq!(identity.write_id(&mut len, std::ptr::null_mut()), 122);
        let required = len;
        let mut buffer = vec![0xabu8; len as usize + 2];
        len -= 1;
        assert_eq!(identity.write_id(&mut len, buffer.as_mut_ptr()), 122);
        assert!(buffer.iter().all(|b| *b == 0xab));
        assert_eq!(len, required);
        // Deliberately unaligned output exercises the byte-buffer ABI.
        assert_eq!(identity.write_id(&mut len, buffer.as_mut_ptr().add(1)), 0);
        let id = std::ptr::read_unaligned(buffer.as_ptr().add(1) as *const PACKAGE_ID);
        for ptr in [id.name, id.publisher, id.resourceId, id.publisherId] {
            assert!(
                (ptr as usize)
                    >= buffer.as_ptr().add(1 + std::mem::size_of::<PACKAGE_ID>()) as usize
            );
            assert!((ptr as usize) < buffer.as_ptr().add(1 + len as usize) as usize);
        }
        assert_eq!(buffer[0], 0xab);
        assert_eq!(buffer[buffer.len() - 1], 0xab);
        let mut len = 0;
        assert_eq!(
            write_string(&identity.full, &mut len, std::ptr::null_mut()),
            122
        );
        assert_eq!(len as usize, identity.full.len());
        let mut text = vec![0; len as usize];
        assert_eq!(write_string(&identity.full, &mut len, text.as_mut_ptr()), 0);
        assert_eq!(text, identity.full);
    }
}

#[test]
fn rejects_missing_identity_wrong_architecture_and_bad_version() {
    for xml in [
        "<Package/>",
        &XML.replace("x64", "arm64"),
        &XML.replace("26.915.4065.0", "26.915.70000.0"),
    ] {
        assert!(Identity::parse(xml).is_err());
    }
}
