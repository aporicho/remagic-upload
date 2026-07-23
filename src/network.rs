use std::collections::BTreeSet;
use std::ffi::CStr;
use std::io;
use std::net::{Ipv4Addr, SocketAddrV4};

pub fn local_urls(port: u16) -> io::Result<Vec<String>> {
    let mut addresses = BTreeSet::new();
    let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
    if unsafe { libc::getifaddrs(&mut list) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let mut current = list;
    while !current.is_null() {
        let interface = unsafe { &*current };
        if !interface.ifa_addr.is_null()
            && unsafe { (*interface.ifa_addr).sa_family as i32 } == libc::AF_INET
        {
            let name = unsafe { CStr::from_ptr(interface.ifa_name) }.to_string_lossy();
            if name != "lo" {
                let address = unsafe { &*(interface.ifa_addr as *const libc::sockaddr_in) };
                let ip = Ipv4Addr::from(u32::from_be(address.sin_addr.s_addr));
                if !ip.is_loopback() && !ip.is_unspecified() && !ip.is_link_local() {
                    addresses.insert(ip);
                }
            }
        }
        current = unsafe { (*current).ifa_next };
    }
    unsafe { libc::freeifaddrs(list) };

    let usb = Ipv4Addr::new(10, 11, 99, 1);
    let mut ordered = addresses.into_iter().collect::<Vec<_>>();
    ordered.sort_by_key(|address| (*address != usb, *address));
    Ok(ordered
        .into_iter()
        .map(|address| format!("http://{}", SocketAddrV4::new(address, port)))
        .collect())
}

#[cfg(test)]
mod tests {
    #[test]
    fn socket_url_format_keeps_the_port() {
        assert_eq!(
            format!(
                "http://{}",
                std::net::SocketAddrV4::new(std::net::Ipv4Addr::new(10, 11, 99, 1), 8787)
            ),
            "http://10.11.99.1:8787"
        );
    }
}
