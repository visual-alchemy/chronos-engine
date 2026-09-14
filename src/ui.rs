pub const HTML: &str = include_str!("../assets/index.html");
pub const CSS: &str = include_str!("../assets/app.css");
pub const JS: &str = include_str!("../assets/app.js");

#[cfg(test)]
mod tests {
    use super::{HTML, JS};
    #[test]
    fn dashboard_contains_media_browser() {
        assert!(HTML.contains("Start listener"));
        assert!(HTML.contains("Active outputs"));
        assert!(HTML.contains("VLC calls this listener"));
    }

    #[test]
    fn dashboard_contains_srt_client_status() {
        assert!(HTML.contains("stream-clients"));
        assert!(JS.contains("No client connected"));
    }

    #[test]
    fn dashboard_formats_multiple_and_ipv6_clients() {
        assert!(JS.contains("Clients"));
        assert!(JS.contains("client.ip.includes(':')"));
        assert!(JS.contains("[${client.ip}]:${client.port}"));
    }
}
