/* MicYou — Turns your Android device into a high-quality PC microphone. */

use mdns_sd::{ServiceDaemon, ServiceInfo};
use micyou_protocol::MDNS_SERVICE_TYPE;
use std::collections::HashMap;

pub struct NetworkManager {
    mdns: ServiceDaemon,
    service_fullname: String,
}

impl NetworkManager {
    pub fn start_mdns(port: u16, bind_address: &str) -> Result<Self, Box<dyn std::error::Error>> {
        Self::start_mdns_helper(bind_address, port, MDNS_SERVICE_TYPE)
    }

    pub fn start_web_mdns(
        port: u16,
        bind_address: &str,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        Self::start_mdns_helper(bind_address, port, micyou_protocol::MDNS_WEB_SERVICE_TYPE)
    }

    pub fn stop_mdns(&self) {
        let _ = self.mdns.unregister(&self.service_fullname);
        let _ = self.mdns.shutdown();
    }

    fn get_best_ip() -> Option<String> {
        // host_info already scores and sorts candidates (v4 home ranges
        // first for stock-client compatibility, then v6 global/ULA).
        crate::host_info::query_network_interfaces()
            .first()
            .map(|i| i.ip.clone())
            .or_else(|| local_ip_address::local_ip().ok().map(|ip| ip.to_string()))
    }

    fn start_mdns_helper(
        bind_address: &str,
        port: u16,
        service_type: &str,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let mdns = ServiceDaemon::new()?;

        let host_name = hostname::get()?
            .into_string()
            .unwrap_or_else(|_| "UnknownHost".to_string());
        let instance_name = format!("MicYou ({})", host_name);

        let local_ip = match bind_address.trim() {
            "" | "auto" | "*" | "0.0.0.0" | "::" => {
                Self::get_best_ip().unwrap_or_else(|| "127.0.0.1".to_string())
            }
            specific => specific.to_string(),
        };

        let service_fullname = format!("{}.{}", instance_name, service_type);

        // Hostname must be a valid DNS name, e.g. "mycomputer.local."
        let valid_host_name = format!("{}.local.", host_name.replace(" ", "-"));

        // Setup mDNS service info
        let properties: HashMap<String, String> = HashMap::new();
        let service_info = ServiceInfo::new(
            service_type,
            &instance_name,
            &valid_host_name,
            local_ip.to_string(),
            port,
            Some(properties),
        )?;

        // Register the service
        mdns.register(service_info)?;
        log::info!("mDNS Service registered: {}", service_fullname);

        Ok(Self {
            mdns,
            service_fullname,
        })
    }
}
