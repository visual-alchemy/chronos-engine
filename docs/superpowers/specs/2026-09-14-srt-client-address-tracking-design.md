# SRT Client Address Tracking Design

## Goal

Show which SRT callers are currently connected to each CHRONOS output. Each caller is identified only by its remote IP address and source port.

## Scope

- Track currently connected callers in process memory.
- Support more than one simultaneous caller per stream.
- Expose callers through the existing stream REST responses and WebSocket events.
- Show the callers on each active-output card in the dashboard.
- Remove a caller when GStreamer's `caller-removed` signal fires.
- Clear all caller data when a stream stops or CHRONOS restarts.

Connection history, timestamps, database persistence, authentication, and geographic lookup are out of scope.

## Data Model

Add a serialized client-address value containing:

```json
{
  "ip": "192.168.1.42",
  "port": 54321
}
```

Each `StreamEvent` contains a `clients` array. An empty array means no caller is connected.

## Runtime Design

The supervisor attaches handlers to the `srtsink` element's `caller-added` and `caller-removed` signals before starting a pipeline. A handler converts the supplied `GSocketAddress` into the API-safe client-address type, updates the matching managed stream under the supervisor mutex, and emits a fresh stream event.

Duplicate add signals for the same IP and port do not create duplicate entries. Removal deletes the matching entry. A malformed or unsupported socket address is ignored and logged without affecting playout.

## API and UI

`GET /api/streams`, `GET /api/streams/{id}`, creation responses, stop responses, and WebSocket stream events use the same `StreamEvent` shape and therefore include `clients`.

The active-output card displays:

- `No client connected` when `clients` is empty.
- `Client: IP:port` for one caller.
- A compact list of `IP:port` values for multiple callers.

The browser refreshes the card through its existing WebSocket event handling.

## Testing

- Unit-test adding a client, suppressing duplicates, and removing a client from supervisor state.
- Unit-test JSON serialization of the new field.
- Keep the existing GStreamer pipeline, API, and dashboard tests passing.
- Manually connect and disconnect an SRT caller and verify that the API reflects both transitions.
