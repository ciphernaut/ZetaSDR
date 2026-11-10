//! Where a USB radio is attached: its bus, port path and negotiated speed, found from
//! the device's serial in the kernel's sysfs tree.

use std::path::Path;

/// `usb:<bus>-<devpath>@<speed>` (`usb:2-1.4@5000`, the speed in Mbit/s as the kernel
/// names it) for the USB device whose `serial` file holds `serial`, from that device's
/// `busnum`, `devpath` and `speed` files under `<sysfs>/bus/usb/devices/`; `None` when
/// no device matches, the serial is empty, a file is missing, or the tree is absent (a
/// host without sysfs). The speed is what [`zeta_core::port::link_speed`] reads back.
pub fn usb_attachment(sysfs: &Path, serial: &str) -> Option<String> {
    let serial = serial.trim();
    if serial.is_empty() {
        return None;
    }
    let read = |dir: &Path, name: &str| {
        std::fs::read_to_string(dir.join(name))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    };
    let mut dirs: Vec<_> = std::fs::read_dir(sysfs.join("bus/usb/devices"))
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    // The same answer on every read of a tree with two matches.
    dirs.sort();
    dirs.iter().find_map(|dir| {
        if read(dir, "serial")? != serial {
            return None;
        }
        let (bus, path, speed) = (
            read(dir, "busnum")?,
            read(dir, "devpath")?,
            read(dir, "speed")?,
        );
        Some(format!("usb:{bus}-{path}@{speed}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A device directory `name` under `root`'s `bus/usb/devices`, with these files.
    fn device(root: &Path, name: &str, files: &[(&str, &str)]) {
        let dir = root.join("bus/usb/devices").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        for (f, v) in files {
            std::fs::write(dir.join(f), format!("{v}\n")).unwrap();
        }
    }

    #[test]
    fn a_serial_is_found_and_reported_with_its_port_and_speed() {
        let root = tempfile::tempdir().unwrap();
        device(
            root.path(),
            "1-2",
            &[
                ("serial", "OTHER"),
                ("busnum", "1"),
                ("devpath", "2"),
                ("speed", "480"),
            ],
        );
        device(
            root.path(),
            "2-1.4",
            &[
                ("serial", "31B9F2A"),
                ("busnum", "2"),
                ("devpath", "1.4"),
                ("speed", "5000"),
            ],
        );
        // An interface directory has no serial.
        device(root.path(), "2-1.4:1.0", &[("bInterfaceClass", "ff")]);
        assert_eq!(
            usb_attachment(root.path(), "31B9F2A").as_deref(),
            Some("usb:2-1.4@5000")
        );
        assert_eq!(
            usb_attachment(root.path(), "OTHER").as_deref(),
            Some("usb:1-2@480")
        );
    }

    #[test]
    fn an_unknown_serial_or_a_missing_tree_is_none() {
        let root = tempfile::tempdir().unwrap();
        device(
            root.path(),
            "1-2",
            &[
                ("serial", "ABC"),
                ("busnum", "1"),
                ("devpath", "2"),
                ("speed", "480"),
            ],
        );
        assert_eq!(usb_attachment(root.path(), "XYZ"), None);
        assert_eq!(usb_attachment(root.path(), ""), None);
        assert_eq!(usb_attachment(&root.path().join("absent"), "ABC"), None);
        // A device whose speed cannot be read is not reported.
        device(
            root.path(),
            "3-1",
            &[("serial", "NOSPEED"), ("busnum", "3")],
        );
        assert_eq!(usb_attachment(root.path(), "NOSPEED"), None);
    }
}
