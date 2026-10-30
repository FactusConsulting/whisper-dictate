use super::*;
use crate::injection::paste::vk;

/// Records every chord step and fails the first step matching `fail_on`.
#[derive(Default)]
struct FakeSink {
    events: Vec<(u16, ChordStep)>,
    fail_on: Option<(u16, ChordStep)>,
}

impl ChordKeySink for FakeSink {
    fn send(&mut self, vk: u16, step: ChordStep) -> Result<()> {
        self.events.push((vk, step));
        if self.fail_on == Some((vk, step)) {
            return Err(anyhow!("fake failure on {vk:#x} {step:?}"));
        }
        Ok(())
    }
}

#[test]
fn chord_presses_clicks_then_releases_in_reverse() {
    let mut sink = FakeSink::default();
    run_key_chord(&mut sink, &[vk::VK_CONTROL, vk::VK_SHIFT], vk::VK_V).unwrap();
    assert_eq!(
        sink.events,
        vec![
            (vk::VK_CONTROL, ChordStep::Press),
            (vk::VK_SHIFT, ChordStep::Press),
            (vk::VK_V, ChordStep::Click),
            (vk::VK_SHIFT, ChordStep::Release),
            (vk::VK_CONTROL, ChordStep::Release),
        ]
    );
}

#[test]
fn failed_main_key_releases_every_pressed_modifier() {
    let mut sink = FakeSink {
        fail_on: Some((vk::VK_V, ChordStep::Click)),
        ..FakeSink::default()
    };
    let err = run_key_chord(&mut sink, &[vk::VK_CONTROL, vk::VK_SHIFT], vk::VK_V).unwrap_err();
    assert!(err.to_string().contains("fake failure"), "{err}");
    assert_eq!(
        sink.events,
        vec![
            (vk::VK_CONTROL, ChordStep::Press),
            (vk::VK_SHIFT, ChordStep::Press),
            (vk::VK_V, ChordStep::Click),
            (vk::VK_SHIFT, ChordStep::Release),
            (vk::VK_CONTROL, ChordStep::Release),
        ]
    );
}

#[test]
fn failed_modifier_press_releases_only_earlier_modifiers() {
    let mut sink = FakeSink {
        fail_on: Some((vk::VK_SHIFT, ChordStep::Press)),
        ..FakeSink::default()
    };
    run_key_chord(&mut sink, &[vk::VK_CONTROL, vk::VK_SHIFT], vk::VK_V).unwrap_err();
    assert_eq!(
        sink.events,
        vec![
            (vk::VK_CONTROL, ChordStep::Press),
            (vk::VK_SHIFT, ChordStep::Press),
            (vk::VK_CONTROL, ChordStep::Release),
        ]
    );
}

#[test]
fn failed_release_still_releases_remaining_modifiers() {
    let mut sink = FakeSink {
        fail_on: Some((vk::VK_SHIFT, ChordStep::Release)),
        ..FakeSink::default()
    };
    run_key_chord(&mut sink, &[vk::VK_CONTROL, vk::VK_SHIFT], vk::VK_V).unwrap_err();
    assert_eq!(
        sink.events.last(),
        Some(&(vk::VK_CONTROL, ChordStep::Release))
    );
}

#[cfg(all(feature = "rust-injection", target_os = "windows"))]
#[test]
fn paste_keys_map_to_raw_virtual_keys_on_windows() {
    use enigo::Key;
    assert_eq!(enigo_impl::vk_to_enigo(vk::VK_V).unwrap(), Key::Other(0x56));
    assert_eq!(
        enigo_impl::vk_to_enigo(vk::VK_INSERT).unwrap(),
        Key::Other(0x2D)
    );
}
