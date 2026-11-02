use super::{plan_format, FormatPlan};

const CF_UNICODETEXT: u32 = 13;
const HTML_FORMAT: u32 = 0xC0F1;

#[test]
fn memory_formats_are_copied() {
    for format in [
        CF_UNICODETEXT,
        super::CF_DIB,
        super::CF_DIBV5,
        15,
        HTML_FORMAT,
    ] {
        assert_eq!(
            plan_format(format, &[format]),
            FormatPlan::Memory,
            "{format:#x}"
        );
    }
}

#[test]
fn bitmap_and_palette_are_skipped_only_when_a_dib_can_rebuild_them() {
    for source in [super::CF_DIB, super::CF_DIBV5] {
        let present = [super::CF_BITMAP, super::CF_PALETTE, source];
        assert_eq!(plan_format(super::CF_BITMAP, &present), FormatPlan::Skip);
        assert_eq!(plan_format(super::CF_PALETTE, &present), FormatPlan::Skip);
    }
    let present = [super::CF_BITMAP, super::CF_PALETTE];
    assert_eq!(
        plan_format(super::CF_BITMAP, &present),
        FormatPlan::Unsupported
    );
    assert_eq!(
        plan_format(super::CF_PALETTE, &present),
        FormatPlan::Unsupported
    );
}

#[test]
fn metafile_picture_is_skipped_only_beside_an_enhanced_metafile() {
    let present = [super::CF_METAFILEPICT, super::CF_ENHMETAFILE];
    assert_eq!(
        plan_format(super::CF_ENHMETAFILE, &present),
        FormatPlan::EnhancedMetafile
    );
    assert_eq!(
        plan_format(super::CF_METAFILEPICT, &present),
        FormatPlan::Skip
    );
    assert_eq!(
        plan_format(super::CF_METAFILEPICT, &[super::CF_METAFILEPICT]),
        FormatPlan::Unsupported
    );
}

#[test]
fn display_private_and_gdi_object_formats_are_unsupported() {
    for format in [
        super::CF_OWNERDISPLAY,
        super::CF_DSPBITMAP,
        super::CF_DSPMETAFILEPICT,
        super::CF_DSPENHMETAFILE,
        0x200,
        0x2FF,
        0x300,
        0x3FF,
    ] {
        assert_eq!(
            plan_format(format, &[format]),
            FormatPlan::Unsupported,
            "{format:#x}"
        );
    }
    assert_eq!(plan_format(0x400, &[0x400]), FormatPlan::Memory);
}

/// Manual-only: drives the process-global clipboard. Saves the user's
/// selection first and skips when that selection itself cannot be saved.
#[cfg(target_os = "windows")]
#[test]
#[ignore = "writes the real system clipboard; run deliberately on a disposable session"]
fn rich_selection_round_trips_through_snapshot_and_restore() {
    use super::{restore, snapshot};
    use crate::injection::{SavedFormat, SavedFormatKind};
    use windows_sys::Win32::Graphics::Gdi::{
        CloseEnhMetaFile, CreateEnhMetaFileW, DeleteEnhMetaFile, GetEnhMetaFileBits, Rectangle,
    };
    use windows_sys::Win32::System::DataExchange::RegisterClipboardFormatW;

    let Some(user_selection) = snapshot() else {
        eprintln!("skipped: the current clipboard selection cannot be saved");
        return;
    };
    struct Restore(Vec<SavedFormat>);
    impl Drop for Restore {
        fn drop(&mut self) {
            restore(&self.0);
        }
    }
    let _restore_user = Restore(user_selection);

    let register = |name: &str| {
        let wide: Vec<u16> = name.encode_utf16().chain([0]).collect();
        unsafe { RegisterClipboardFormatW(wide.as_ptr()) }
    };
    let memory = |format: u32, bytes: Vec<u8>| SavedFormat {
        format,
        kind: SavedFormatKind::Memory,
        bytes,
    };
    let text: Vec<u8> = "rich ÆØÅ\0"
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    let metafile = unsafe {
        let dc = CreateEnhMetaFileW(
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
        );
        Rectangle(dc, 0, 0, 40, 20);
        let handle = CloseEnhMetaFile(dc);
        let size = GetEnhMetaFileBits(handle, 0, std::ptr::null_mut());
        let mut bytes = vec![0u8; size as usize];
        GetEnhMetaFileBits(handle, size, bytes.as_mut_ptr());
        DeleteEnhMetaFile(handle);
        bytes
    };
    let rich = vec![
        memory(
            register("HTML Format"),
            b"Version:0.9\r\n<b>rich</b>\0".to_vec(),
        ),
        memory(register("WhisperDictateTestFormat"), vec![0, 1, 2, 0, 255]),
        memory(CF_UNICODETEXT, text),
        SavedFormat {
            format: super::CF_ENHMETAFILE,
            kind: SavedFormatKind::EnhancedMetafile,
            bytes: metafile,
        },
    ];

    assert!(restore(&rich), "seed the rich selection");
    let saved = snapshot().expect("rich selection must be restorable");
    assert!(restore(&[memory(CF_UNICODETEXT, vec![b'x', 0, 0, 0])]));
    assert!(restore(&saved), "put the rich selection back");
    let after = snapshot().expect("restored selection is readable");

    for expected in &rich {
        let found = after
            .iter()
            .find(|saved| saved.format == expected.format)
            .unwrap_or_else(|| panic!("format {:#x} missing after restore", expected.format));
        assert_eq!(found.kind, expected.kind);
        assert!(
            found.bytes.starts_with(&expected.bytes),
            "format {:#x} changed",
            expected.format
        );
    }
}
