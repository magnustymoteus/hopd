//! Rust bindings generated at compile time from the vendored Hyprland protocol XML.

pub mod global_shortcuts {
    // The generated code refers to `super::wayland_client`.
    use wayland_client;

    pub mod __interfaces {
        wayland_scanner::generate_interfaces!("protocols/hyprland-global-shortcuts-v1.xml");
    }
    use self::__interfaces::*;

    wayland_scanner::generate_client_code!("protocols/hyprland-global-shortcuts-v1.xml");
}
