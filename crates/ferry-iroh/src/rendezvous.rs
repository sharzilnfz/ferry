


use std::io;

use crate::config::MdnsSetting;


pub fn topic_for_code(code: &str) -> String {
    format!("ferry-pair-{}", code.to_ascii_lowercase())
}


pub fn service_name_for_code(code: &str) -> String {
    format!("ferry-pair-{}", code.to_ascii_uppercase())
}




pub fn advertise(code: &str, mdns: Option<&MdnsSetting>) -> io::Result<()> {
    
    
    let _ = (code, mdns);
    Ok(())
}



pub fn discover(
    code: &str,
    mdns: Option<&MdnsSetting>,
) -> io::Result<Option<std::net::SocketAddr>> {
    let _ = (code, mdns);
    Ok(None)
}
