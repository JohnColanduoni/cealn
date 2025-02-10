mod udp_socket;

cfg_if::cfg_if! {
    if #[cfg(target_os = "linux")] {
        #[path = "platform/linux/mod.rs"]
        mod platform;
    } else if #[cfg(target_os = "macos")] {
        #[path = "platform/macos/mod.rs"]
        mod platform;
    } else {
        compile_error!("unsupported platform");
    }
}

pub use self::udp_socket::UdpSocket;
