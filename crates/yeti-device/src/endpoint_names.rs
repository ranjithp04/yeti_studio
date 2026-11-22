//! OS endpoint-name matching shared by the platform backends and the engine's routing code.
//!
//! Owner: **Windows engineer** (Phase 6). Pure string logic, compiled on every platform so it
//! can be unit-tested on the Mac; the Windows backend ([`crate::windows`]) and
//! `yeti-engine`'s Windows routing use it.
//!
//! Windows naming (MMDevice API):
//! - `PKEY_Device_FriendlyName` = "<endpoint description> (<interface name>)", e.g.
//!   "Microphone (Yeti Stereo Microphone)". The user can rename the description part.
//! - `PKEY_DeviceInterface_FriendlyName` = the adapter/interface name, e.g.
//!   "Yeti Stereo Microphone" or "VB-Audio Virtual Cable". Not user-renamable, so preferred.

/// VB-CABLE playback endpoint the engine writes the cleaned voice to (prefix of its friendly name).
pub const VB_CABLE_INPUT: &str = "CABLE Input";
/// VB-CABLE recording endpoint that chat/meeting apps must select as their microphone.
pub const VB_CABLE_OUTPUT: &str = "CABLE Output";
/// Interface (driver) name of VB-Audio's free virtual cable.
pub const VB_CABLE_INTERFACE: &str = "VB-Audio Virtual Cable";
/// Where the user downloads VB-CABLE (free, installs its own signed driver).
pub const VB_CABLE_DOWNLOAD_URL: &str = "https://vb-audio.com/Cable/";
/// Message for [`yeti_types::YetiError::VirtualMicMissing`] / `EngineEvent::Warning` when the
/// cable is absent. The UI shows this as the "Install VB-CABLE" state.
pub const VB_CABLE_MISSING_MESSAGE: &str = "Install VB-CABLE (https://vb-audio.com/Cable/), reboot, \
     then select \"CABLE Output (VB-Audio Virtual Cable)\" as the microphone in your chat or meeting app";

fn contains_ci(haystack: &str, needle: &str) -> bool {
    haystack.to_lowercase().contains(&needle.to_lowercase())
}

/// `true` if an endpoint with this friendly name / interface name belongs to the Blue Yeti.
///
/// Matches any of [`crate::YETI_NAMES`] case-insensitively in either name, so a user-renamed
/// endpoint ("Streaming Mic (Yeti Stereo Microphone)") is still found.
pub fn is_yeti_endpoint(friendly_name: &str, interface_name: &str) -> bool {
    crate::is_yeti_name(interface_name) || crate::is_yeti_name(friendly_name)
}

/// `true` if a **render** endpoint is VB-CABLE's "CABLE Input".
///
/// Only the plain VB-CABLE is accepted: VB-Audio's other products ("CABLE-A Input (VB-Audio
/// Cable A)", "Hi-Fi Cable Input (VB-Audio Hi-Fi Cable)") use different names on purpose so they
/// are not picked up by accident.
pub fn is_vb_cable_input(friendly_name: &str, interface_name: &str) -> bool {
    // VB-CABLE also installs "CABLE In 16ch" on the same interface: a 16-channel endpoint
    // that is not the stereo cable chat apps record from. Never pick it.
    if friendly_name.to_lowercase().contains("16ch") {
        return false;
    }
    let iface = interface_name.trim();
    if !iface.is_empty() {
        return iface.eq_ignore_ascii_case(VB_CABLE_INTERFACE);
    }
    // Interface name unavailable: fall back to the full default friendly name.
    let f = friendly_name.trim().to_lowercase();
    f.starts_with(&VB_CABLE_INPUT.to_lowercase())
        && f.contains(&format!("({})", VB_CABLE_INTERFACE.to_lowercase()))
}

/// `true` if `name` selects the endpoint called `friendly_name`: exact (case-insensitive) match,
/// or `name` is a case-insensitive substring of it. Used for `EngineConfig` device overrides.
pub fn name_selects(name: &str, friendly_name: &str) -> bool {
    let n = name.trim();
    !n.is_empty() && (friendly_name.eq_ignore_ascii_case(n) || contains_ci(friendly_name, n))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vb_cable_16ch_endpoint_is_rejected() {
        assert!(is_vb_cable_input(
            "CABLE Input (VB-Audio Virtual Cable)",
            "VB-Audio Virtual Cable"
        ));
        assert!(!is_vb_cable_input(
            "CABLE In 16ch (VB-Audio Virtual Cable)",
            "VB-Audio Virtual Cable"
        ));
        assert!(!is_vb_cable_input(
            "CABLE In 16ch (VB-Audio Virtual Cable)",
            ""
        ));
    }

    #[test]
    fn yeti_matches_default_and_renamed_endpoints() {
        assert!(is_yeti_endpoint(
            "Microphone (Yeti Stereo Microphone)",
            "Yeti Stereo Microphone"
        ));
        assert!(is_yeti_endpoint(
            "Speakers (Yeti Stereo Microphone)",
            "Yeti Stereo Microphone"
        ));
        // User renamed the description; interface name still identifies it.
        assert!(is_yeti_endpoint("Streaming Mic", "Yeti Stereo Microphone"));
        // Windows with Logitech's driver names the same Yeti (046d:0ab7) "Yeti Classic".
        assert!(is_yeti_endpoint("Microphone (Yeti Classic)", ""));
        assert!(is_yeti_endpoint("Speakers (Yeti Classic)", ""));
        assert!(!is_yeti_endpoint("Microphone (HD Pro Webcam C920)", ""));
        assert!(!is_yeti_endpoint("Yeti Studio Mic", ""));
        // Interface name missing; friendly name still carries it.
        assert!(is_yeti_endpoint("Microphone (YETI STEREO MICROPHONE)", ""));
        assert!(!is_yeti_endpoint(
            "Microphone (Realtek(R) Audio)",
            "Realtek(R) Audio"
        ));
        assert!(!is_yeti_endpoint(
            "CABLE Output (VB-Audio Virtual Cable)",
            "VB-Audio Virtual Cable"
        ));
    }

    #[test]
    fn vb_cable_input_matching() {
        assert!(is_vb_cable_input(
            "CABLE Input (VB-Audio Virtual Cable)",
            "VB-Audio Virtual Cable"
        ));
        assert!(is_vb_cable_input("My cable", "vb-audio virtual cable"));
        assert!(is_vb_cable_input(
            "CABLE Input (VB-Audio Virtual Cable)",
            ""
        ));
        assert!(!is_vb_cable_input(
            "CABLE-A Input (VB-Audio Cable A)",
            "VB-Audio Cable A"
        ));
        assert!(!is_vb_cable_input(
            "Hi-Fi Cable Input (VB-Audio Hi-Fi Cable)",
            "VB-Audio Hi-Fi Cable"
        ));
        assert!(!is_vb_cable_input("CABLE-A Input (VB-Audio Cable A)", ""));
        assert!(!is_vb_cable_input(
            "Speakers (Realtek(R) Audio)",
            "Realtek(R) Audio"
        ));
    }

    #[test]
    fn name_selection() {
        assert!(name_selects(
            "CABLE Input",
            "CABLE Input (VB-Audio Virtual Cable)"
        ));
        assert!(name_selects(
            "cable input (vb-audio virtual cable)",
            "CABLE Input (VB-Audio Virtual Cable)"
        ));
        assert!(!name_selects("", "CABLE Input (VB-Audio Virtual Cable)"));
        assert!(!name_selects("Yeti", "Speakers (Realtek(R) Audio)"));
    }

    #[test]
    fn missing_message_mentions_install_and_url() {
        assert!(VB_CABLE_MISSING_MESSAGE.starts_with("Install VB-CABLE"));
        assert!(VB_CABLE_MISSING_MESSAGE.contains(VB_CABLE_DOWNLOAD_URL));
        assert!(VB_CABLE_MISSING_MESSAGE.contains(VB_CABLE_OUTPUT));
    }
}
