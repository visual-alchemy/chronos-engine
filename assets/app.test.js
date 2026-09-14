const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const test = require('node:test');
const vm = require('node:vm');

const source = fs.readFileSync(path.join(__dirname, 'app.js'), 'utf8');
const formatterSource = source.slice(
  source.indexOf('function callerUrl'),
  source.indexOf('function getTimestamp'),
);
const context = { URL };
vm.runInNewContext(
  `${formatterSource}\nglobalThis.formatClientSummary = formatClientSummary;`,
  context,
);
const { formatClientSummary } = context;

test('formats valid IPv4 and IPv6 client addresses', () => {
  assert.equal(
    formatClientSummary([
      { ip: ' 192.0.2.10 ', port: 41000 },
      { ip: '2001:db8::10', port: 42000 },
    ]),
    'Clients: 192.0.2.10:41000, [2001:db8::10]:42000',
  );
});

test('ignores entries without an IPv4 or IPv6 literal', () => {
  assert.equal(
    formatClientSummary([
      { ip: 'caller.example.com', port: 41000 },
      { ip: '256.0.2.10', port: 41001 },
      { ip: '2001:db8::not-hex', port: 41002 },
      { ip: '', port: 41003 },
      { ip: null, port: 41004 },
    ]),
    'No client connected',
  );
});

test('requires an integer source port from 1 through 65535', () => {
  assert.equal(
    formatClientSummary([
      { ip: '192.0.2.10', port: 0 },
      { ip: '192.0.2.11', port: -1 },
      { ip: '192.0.2.12', port: 1.5 },
      { ip: '192.0.2.13', port: '41000' },
      { ip: '192.0.2.14', port: 65536 },
      { ip: '192.0.2.15', port: 65535 },
    ]),
    'Client: 192.0.2.15:65535',
  );
});

test('returns the empty state for non-array input', () => {
  assert.equal(formatClientSummary(null), 'No client connected');
  assert.equal(formatClientSummary({}), 'No client connected');
});
