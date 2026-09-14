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
        assert!(JS.contains("function formatClientSummary(value)"));
        assert!(JS.contains("clientsLine.textContent = formatClientSummary(stream.clients);"));
        assert!(JS.contains("Array.isArray(value)"));
        assert!(JS.contains("Number.isInteger(client.port)"));
        assert!(JS.contains("? 'Client' : 'Clients'"));
        assert!(JS.contains(": ${addresses.join(', ')}"));
        assert!(JS.contains("ip.includes(':')"));
        assert!(JS.contains("[${ip}]:${client.port}"));
    }
}
