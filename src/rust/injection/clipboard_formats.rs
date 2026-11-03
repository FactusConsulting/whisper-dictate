//! Full-format Windows clipboard snapshot and restore for paste injection.
//!
//! Paste injection replaces the user's selection with the transcript and puts
//! the selection back afterwards. A rich selection (HTML, RTF, images, Office
//! data) is saved format by format: global-memory blocks as raw bytes,
//! enhanced metafiles as their serialized bits. Bitmap, palette and
//! metafile-picture handles are skipped when Windows can synthesize them from
//! a saved format. Any other handle-based or private format, or a selection
//! larger than `MAX_SNAPSHOT_BYTES`, makes the snapshot fail so the caller
//! leaves the clipboard untouched.

#[cfg(target_os = "windows")]
use super::paste::{SavedFormat, SavedFormatKind};

/// Upper bound on the bytes held for one restore.
#[cfg(target_os = "windows")]
pub(crate) const MAX_SNAPSHOT_BYTES: usize = 32 * 1024 * 1024;

const CF_BITMAP: u32 = 2;
const CF_METAFILEPICT: u32 = 3;
const CF_DIB: u32 = 8;
const CF_PALETTE: u32 = 9;
const CF_ENHMETAFILE: u32 = 14;
const CF_DIBV5: u32 = 17;
const CF_OWNERDISPLAY: u32 = 0x80;
const CF_DSPBITMAP: u32 = 0x82;
const CF_DSPMETAFILEPICT: u32 = 0x83;
const CF_DSPENHMETAFILE: u32 = 0x8E;
const CF_PRIVATEFIRST: u32 = 0x200;
const CF_GDIOBJLAST: u32 = 0x3FF;

/// How one present clipboard format is handled by the snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FormatPlan {
    /// Copy the global-memory block.
    Memory,
    /// Serialize the enhanced metafile.
    EnhancedMetafile,
    /// Windows re-synthesizes it from another saved format.
    Skip,
    /// Cannot be saved; the whole snapshot must fail.
    Unsupported,
}

/// Decide how to save `format` given every format present on the clipboard.
pub(crate) fn plan_format(format: u32, present: &[u32]) -> FormatPlan {
    let has = |wanted: u32| present.contains(&wanted);
    match format {
        CF_BITMAP | CF_PALETTE if has(CF_DIB) || has(CF_DIBV5) => FormatPlan::Skip,
        CF_METAFILEPICT if has(CF_ENHMETAFILE) => FormatPlan::Skip,
        CF_ENHMETAFILE => FormatPlan::EnhancedMetafile,
        CF_BITMAP | CF_PALETTE | CF_METAFILEPICT => FormatPlan::Unsupported,
        CF_OWNERDISPLAY | CF_DSPBITMAP | CF_DSPMETAFILEPICT | CF_DSPENHMETAFILE => {
            FormatPlan::Unsupported
        }
        CF_PRIVATEFIRST..=CF_GDIOBJLAST => FormatPlan::Unsupported,
        _ => FormatPlan::Memory,
    }
}

#[cfg(target_os = "windows")]
mod win32 {
    use super::*;
    use windows_sys::Win32::Foundation::{GlobalFree, HANDLE, HWND};
    use windows_sys::Win32::Graphics::Gdi::{
        DeleteEnhMetaFile, GetEnhMetaFileBits, SetEnhMetaFileBits,
    };
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, EnumClipboardFormats, GetClipboardData, OpenClipboard,
        SetClipboardData,
    };
    use windows_sys::Win32::System::Memory::{
        GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DestroyWindow, HWND_MESSAGE,
    };

    /// Message-only window that owns the clipboard during a restore:
    /// `EmptyClipboard` after `OpenClipboard(NULL)` leaves the clipboard
    /// without an owner, which the Win32 contract says makes
    /// `SetClipboardData` fail.
    struct OwnerWindow(HWND);

    impl OwnerWindow {
        fn create() -> Option<Self> {
            let class: Vec<u16> = "STATIC\0".encode_utf16().collect();
            // SAFETY: plain message-only window creation on this thread.
            let hwnd = unsafe {
                CreateWindowExW(
                    0,
                    class.as_ptr(),
                    std::ptr::null(),
                    0,
                    0,
                    0,
                    0,
                    0,
                    HWND_MESSAGE,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null(),
                )
            };
            (!hwnd.is_null()).then_some(Self(hwnd))
        }
    }

    impl Drop for OwnerWindow {
        fn drop(&mut self) {
            // SAFETY: created on this thread and destroyed exactly once.
            unsafe { DestroyWindow(self.0) };
        }
    }

    /// Open clipboard, closed on drop. Clipboard managers, RDP and the app
    /// being pasted into can hold it briefly; retry for up to ~500 ms.
    struct OpenedClipboard;

    impl OpenedClipboard {
        fn open(owner: HWND) -> Option<Self> {
            for attempt in 0..25 {
                if unsafe { OpenClipboard(owner) } != 0 {
                    return Some(Self);
                }
                if attempt < 24 {
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
            }
            None
        }
    }

    impl Drop for OpenedClipboard {
        fn drop(&mut self) {
            unsafe {
                CloseClipboard();
            }
        }
    }

    /// Save every format of the current selection, or `None` when any
    /// present format cannot be saved.
    pub(crate) fn snapshot() -> Option<Vec<SavedFormat>> {
        // Reading needs no owner window: the clipboard is never emptied here.
        let _clipboard = OpenedClipboard::open(std::ptr::null_mut())?;
        let mut present = Vec::new();
        let mut format = 0;
        loop {
            format = unsafe { EnumClipboardFormats(format) };
            if format == 0 {
                break;
            }
            present.push(format);
        }
        let mut saved = Vec::with_capacity(present.len());
        let mut total = 0usize;
        for &format in &present {
            let (kind, bytes) = match plan_format(format, &present) {
                FormatPlan::Skip => continue,
                FormatPlan::Unsupported => return None,
                FormatPlan::Memory => (SavedFormatKind::Memory, read_memory(format)?),
                FormatPlan::EnhancedMetafile => (
                    SavedFormatKind::EnhancedMetafile,
                    read_enhanced_metafile(format)?,
                ),
            };
            total = total.checked_add(bytes.len())?;
            if total > MAX_SNAPSHOT_BYTES {
                return None;
            }
            saved.push(SavedFormat {
                format,
                kind,
                bytes,
            });
        }
        Some(saved)
    }

    fn read_memory(format: u32) -> Option<Vec<u8>> {
        unsafe {
            let handle = GetClipboardData(format);
            if handle.is_null() {
                return None;
            }
            let size = GlobalSize(handle);
            if size == 0 || size > MAX_SNAPSHOT_BYTES {
                return None;
            }
            let data = GlobalLock(handle) as *const u8;
            if data.is_null() {
                return None;
            }
            let bytes = std::slice::from_raw_parts(data, size).to_vec();
            GlobalUnlock(handle);
            Some(bytes)
        }
    }

    fn read_enhanced_metafile(format: u32) -> Option<Vec<u8>> {
        unsafe {
            let handle = GetClipboardData(format);
            if handle.is_null() {
                return None;
            }
            let size = GetEnhMetaFileBits(handle, 0, std::ptr::null_mut());
            if size == 0 || size as usize > MAX_SNAPSHOT_BYTES {
                return None;
            }
            let mut bytes = vec![0u8; size as usize];
            if GetEnhMetaFileBits(handle, size, bytes.as_mut_ptr()) != size {
                return None;
            }
            Some(bytes)
        }
    }

    /// Replace the clipboard with `formats`; `true` when every format was set.
    pub(crate) fn restore(formats: &[SavedFormat]) -> bool {
        let Some(owner) = OwnerWindow::create() else {
            return false;
        };
        let all_set = {
            let Some(_clipboard) = OpenedClipboard::open(owner.0) else {
                return false;
            };
            if unsafe { EmptyClipboard() } == 0 {
                return false;
            }
            formats
                .iter()
                .fold(true, |all_set, saved| set_format(saved) && all_set)
        };
        drop(owner);
        all_set
    }

    fn set_format(saved: &SavedFormat) -> bool {
        unsafe {
            match saved.kind {
                SavedFormatKind::Memory => {
                    let handle = GlobalAlloc(GMEM_MOVEABLE, saved.bytes.len());
                    if handle.is_null() {
                        return false;
                    }
                    let data = GlobalLock(handle) as *mut u8;
                    if data.is_null() {
                        GlobalFree(handle);
                        return false;
                    }
                    std::ptr::copy_nonoverlapping(saved.bytes.as_ptr(), data, saved.bytes.len());
                    GlobalUnlock(handle);
                    if SetClipboardData(saved.format, handle).is_null() {
                        GlobalFree(handle);
                        return false;
                    }
                    true
                }
                SavedFormatKind::EnhancedMetafile => {
                    let Ok(size) = u32::try_from(saved.bytes.len()) else {
                        return false;
                    };
                    let metafile = SetEnhMetaFileBits(size, saved.bytes.as_ptr());
                    if metafile.is_null() {
                        return false;
                    }
                    if SetClipboardData(saved.format, metafile as HANDLE).is_null() {
                        DeleteEnhMetaFile(metafile);
                        return false;
                    }
                    true
                }
            }
        }
    }
}

#[cfg(target_os = "windows")]
pub(crate) use win32::{restore, snapshot};

#[cfg(test)]
#[path = "clipboard_formats_tests.rs"]
mod tests;
