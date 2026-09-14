const el = (id) => document.getElementById(id);

const labels = {
  remux_copy: 'Direct stream copy (Zero encode)',
  copy_video_encode_audio: 'Copy video · Encode audio (AAC)',
  encode_video_copy_audio: 'Encode video (H.264) · Copy audio',
  full_transcode: 'Full transcode (H.264 + AAC)',
};

async function api(url, init) {
  const res = await fetch(url, init);
  const body = await res.json().catch(() => ({}));
  if (!res.ok) {
    throw new Error(body.message || `Request failed (${res.status})`);
  }
  return body;
}

function formatBytes(bytes) {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1048576) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1073741824) return `${(bytes / 1048576).toFixed(1)} MB`;
  return `${(bytes / 1073741824).toFixed(2)} GB`;
}

function formatDuration(ms) {
  if (!ms) return null;
  const totalSeconds = Math.floor(ms / 1000);
  const minutes = Math.floor(totalSeconds / 60);
  const seconds = totalSeconds % 60;
  return `${minutes}:${seconds.toString().padStart(2, '0')}`;
}

function show(target, title, lines, error = false) {
  target.hidden = false;
  target.className = `result ${error ? 'error' : ''}`;
  target.replaceChildren();

  const heading = document.createElement('strong');
  heading.textContent = title;

  const ul = document.createElement('ul');
  lines.filter(Boolean).forEach((line) => {
    const li = document.createElement('li');
    li.textContent = line;
    ul.append(li);
  });

  target.append(heading, ul);
}

function callerUrl(host, stream) {
  return `srt://${host}:${stream.port}?mode=caller&latency=${stream.latency_ms || 120}`;
}

function getTimestamp() {
  const now = new Date();
  return now.toTimeString().split(' ')[0];
}

function event(text, isError = false) {
  const list = el('events');
  if (list.firstElementChild && list.firstElementChild.classList.contains('muted')) {
    list.replaceChildren();
  }

  const item = document.createElement('li');
  if (isError) item.className = 'error-text';

  const time = document.createElement('span');
  time.className = 'timestamp';
  time.textContent = `[${getTimestamp()}]`;

  const msg = document.createElement('span');
  msg.textContent = ` ${text}`;

  item.append(time, msg);
  list.prepend(item);

  // Keep last 50 items
  while (list.children.length > 50) {
    list.removeChild(list.lastElementChild);
  }
}

async function copy(text, button) {
  try {
    await navigator.clipboard.writeText(text);
    const originalText = button.dataset.label || button.textContent;
    button.dataset.label = originalText;
    button.textContent = 'Copied!';
    button.style.borderColor = 'var(--emerald)';
    button.style.color = 'var(--emerald)';
    setTimeout(() => {
      button.textContent = originalText;
      button.style.borderColor = '';
      button.style.color = '';
    }, 1400);
  } catch {
    event('Clipboard access unavailable. Select and copy manually.', true);
  }
}

async function loadMedia() {
  try {
    const items = await api('/api/media');
    const root = el('media');
    root.replaceChildren();

    const summary = el('media-summary');
    if (!items.length) {
      summary.textContent = 'No media files found in ./media directory. Mount or copy MP4/MOV files to get started.';
      return;
    }
    summary.textContent = `Found ${items.length} media asset(s). Run Probe to inspect container and codecs before playout.`;

    items.forEach((item, index) => {
      const card = el('media-card').content.firstElementChild.cloneNode(true);
      const result = card.querySelector('.result');
      const probe = card.querySelector('.probe');
      const planButton = card.querySelector('.plan');
      const start = card.querySelector('.start');
      const profile = card.querySelector('.profile');
      const port = card.querySelector('.port');
      const probeBadge = card.querySelector('.probe-badge');
      let plan;

      card.querySelector('.name').textContent = item.filename;
      card.querySelector('.meta').textContent = `${item.path} · ${formatBytes(item.size_bytes)}`;

      if (item.probe_status === 'ready') {
        probeBadge.textContent = 'READY';
        probeBadge.classList.add('ready');
        planButton.disabled = false;
      } else {
        probeBadge.textContent = item.probe_status.toUpperCase();
      }

      port.value = 9000 + index;

      probe.onclick = async () => {
        probe.disabled = true;
        probe.textContent = 'Probing...';
        try {
          const value = await api(`/api/media/${encodeURIComponent(item.id)}/probe`);
          const duration = formatDuration(value.duration_ms);
          const tracks = value.streams.map((s) => {
            if (s.kind === 'video' && s.video) {
              const fps = s.video.framerate_denominator ? (s.video.framerate_numerator / s.video.framerate_denominator).toFixed(1) : '?';
              return `Video: ${s.codec_name || 'unknown'} (${s.video.width}x${s.video.height} @ ${fps} fps)`;
            }
            if (s.kind === 'audio' && s.audio) {
              return `Audio: ${s.codec_name || 'unknown'} (${s.audio.sample_rate} Hz, ${s.audio.channels} ch)`;
            }
            return `${s.kind.toUpperCase()}: ${s.codec_name || 'unknown'}`;
          });

          const summaryLines = [
            value.container_format && `Container: ${value.container_format}`,
            duration && `Duration: ${duration} (${value.duration_ms} ms)`,
            ...tracks,
            value.error?.message,
          ];

          if (value.status === 'ready') {
            probeBadge.textContent = 'READY';
            probeBadge.classList.add('ready');
            show(result, 'Inspection Succeeded (Ready for planning)', summaryLines, false);
            planButton.disabled = false;
          } else {
            probeBadge.textContent = value.status.toUpperCase();
            probeBadge.classList.remove('ready');
            show(result, `Probe: ${value.status}`, summaryLines, true);
          }
        } catch (err) {
          show(result, 'Probe Failed', [err.message], true);
        } finally {
          probe.disabled = false;
          probe.innerHTML = `<svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2"><circle cx="11" cy="11" r="8"></circle><line x1="21" y1="21" x2="16.65" y2="16.65"></line></svg> Probe media`;
        }
      };

      planButton.onclick = async () => {
        planButton.disabled = true;
        planButton.textContent = 'Planning...';
        try {
          plan = await api(`/api/media/${encodeURIComponent(item.id)}/compatibility/${profile.value}`);
          const planTitle = labels[plan.mode] || plan.mode;
          const details = [
            ...plan.reasons,
            `Output Video: ${plan.video_codec.toUpperCase()}`,
            `Output Audio: ${plan.audio_codec.toUpperCase()}`,
          ];
          show(result, planTitle, details, false);
          start.disabled = false;
        } catch (err) {
          show(result, 'Compatibility Check Failed', [err.message], true);
        } finally {
          planButton.disabled = false;
          planButton.innerHTML = `<svg viewBox="0 0 24 24" width="14" height="14" fill="none" stroke="currentColor" stroke-width="2"><path d="M16 4h2a2 2 0 0 1 2 2v14a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2h2"></path><rect x="8" y="2" width="8" height="4" rx="1" ry="1"></rect></svg> Check compatibility`;
        }
      };

      start.onclick = async () => {
        const selectedPort = Number(port.value);
        if (selectedPort < 9000 || selectedPort > 9099) {
          show(result, 'Invalid Port', ['UDP port must be between 9000 and 9099.'], true);
          return;
        }
        start.disabled = true;
        start.textContent = 'Starting...';
        try {
          await api('/api/streams', {
            method: 'POST',
            headers: { 'content-type': 'application/json' },
            body: JSON.stringify({
              id: `stream-${crypto.randomUUID().slice(0, 8)}`,
              media_id: item.id,
              port: selectedPort,
              latency_ms: 120,
              mode: plan ? plan.mode : undefined,
            }),
          });
          event(`SRT listener launched on UDP port ${selectedPort}.`);
          await loadStreams();
        } catch (err) {
          show(result, 'Could not start listener', [err.message], true);
        } finally {
          start.disabled = false;
          start.innerHTML = `<svg viewBox="0 0 24 24" width="16" height="16" fill="currentColor"><polygon points="5 3 19 12 5 21 5 3"></polygon></svg> Start listener`;
        }
      };

      root.append(card);
    });
  } catch (err) {
    el('media-summary').textContent = `Could not load media catalog: ${err.message}`;
  }
}

async function loadStreams() {
  try {
    const streams = await api('/api/streams');
    const root = el('streams');
    const countBadge = el('active-stream-count');
    if (countBadge) countBadge.textContent = `${streams.length} Active`;

    root.replaceChildren();

    if (!streams.length) {
      const empty = document.createElement('div');
      empty.className = 'empty-state';
      empty.textContent = 'No active listeners. Choose an asset from the media panel to create one.';
      root.append(empty);
      return;
    }

    streams.forEach((stream) => {
      const card = el('stream-card').content.firstElementChild.cloneNode(true);
      const local = card.querySelector('.local-url');
      const host = card.querySelector('.lan-host');
      const lan = card.querySelector('.lan-url');
      const pulseDot = card.querySelector('.stream-pulse-dot');
      const stateBadge = card.querySelector('.stream-state');
      const modeBadge = card.querySelector('.stream-mode-badge');
      const clientsLine = card.querySelector('.stream-clients');

      const renderUrls = () => {
        local.value = callerUrl('127.0.0.1', stream);
        lan.value = host.value ? callerUrl(host.value.trim(), stream) : '';
      };

      card.querySelector('.stream-id').textContent = stream.stream_id;
      stateBadge.textContent = stream.state.replaceAll('_', ' ');

      const clientAddresses = (stream.clients || []).map((client) => {
        const address = client.ip.includes(':')
          ? `[${client.ip}]:${client.port}`
          : `${client.ip}:${client.port}`;
        return address;
      });
      clientsLine.textContent = clientAddresses.length === 0
        ? 'No client connected'
        : `${clientAddresses.length === 1 ? 'Client' : 'Clients'}: ${clientAddresses.join(', ')}`;

      if (stream.state === 'running' || stream.state === 'waiting_for_caller') {
        pulseDot.style.background = 'var(--emerald)';
        pulseDot.style.boxShadow = '0 0 10px var(--emerald)';
      } else if (stream.state === 'looping') {
        pulseDot.style.background = 'var(--amber)';
        pulseDot.style.boxShadow = '0 0 10px var(--amber)';
        stateBadge.style.background = 'rgba(245, 158, 11, 0.15)';
        stateBadge.style.color = '#fcd34d';
        stateBadge.style.borderColor = 'rgba(245, 158, 11, 0.3)';
      } else if (stream.state === 'failed') {
        pulseDot.style.background = 'var(--rose)';
        pulseDot.style.boxShadow = '0 0 10px var(--rose)';
        stateBadge.style.background = 'rgba(244, 63, 94, 0.15)';
        stateBadge.style.color = '#fda4af';
        stateBadge.style.borderColor = 'rgba(244, 63, 94, 0.3)';
      }

      const loopCounter = card.querySelector('.loop-counter');
      const loopCount = card.querySelector('.loop-count');
      if (stream.loop_count != null && stream.loop_count > 0) {
        loopCount.textContent = stream.loop_count;
        loopCounter.hidden = false;
      }

      if (modeBadge) {
        modeBadge.textContent = `UDP ${stream.port} · ${labels[stream.mode] || 'Pipeline configured'}`;
      }

      host.value = localStorage.getItem('chronos-lan-host') || '';
      host.oninput = () => {
        localStorage.setItem('chronos-lan-host', host.value.trim());
        renderUrls();
      };
      renderUrls();

      const localButton = card.querySelector('.copy-local');
      const lanButton = card.querySelector('.copy-lan');
      localButton.dataset.label = 'Copy local URL';
      lanButton.dataset.label = 'Copy LAN URL';

      localButton.onclick = () => copy(local.value, localButton);
      lanButton.onclick = () => {
        if (!lan.value) {
          event('Please enter your Mac’s LAN IP first (e.g. 192.168.1.28).', true);
          host.focus();
          return;
        }
        copy(lan.value, lanButton);
      };

      card.querySelector('.stop').onclick = async () => {
        try {
          await api(`/api/streams/${encodeURIComponent(stream.stream_id)}/stop`, { method: 'POST' });
          event(`Stopped playout ${stream.stream_id}.`);
          await loadStreams();
        } catch (err) {
          event(`Failed to stop ${stream.stream_id}: ${err.message}`, true);
        }
      };

      root.append(card);
    });
  } catch (err) {
    event(`Failed to fetch streams: ${err.message}`, true);
  }
}

async function load() {
  const healthDot = el('health-dot');
  const healthVal = el('health');
  try {
    const h = await api('/healthz');
    healthVal.textContent = h.status.toUpperCase();
    healthVal.classList.remove('error');
    if (healthDot) healthDot.classList.remove('error');
    await Promise.all([loadMedia(), loadStreams()]);
  } catch (err) {
    healthVal.textContent = 'OFFLINE';
    healthVal.classList.add('error');
    if (healthDot) healthDot.classList.add('error');
    event(`Engine connection error: ${err.message}`, true);
  }
}

el('refresh').onclick = () => {
  event('Refreshing media catalog...');
  load();
};

const ws = new WebSocket(`${location.protocol === 'https:' ? 'wss' : 'ws'}://${location.host}/api/events`);
ws.onopen = () => {
  event('Connected to CHRONOS event bus.');
};
ws.onmessage = (e) => {
  try {
    const s = JSON.parse(e.data);
    event(`${s.stream_id}: ${s.state.replaceAll('_', ' ')}${s.detail ? ` (${s.detail})` : ''}`);
    loadStreams().catch(() => {});
  } catch {}
};
ws.onerror = () => {
  event('Event bus connection disrupted.', true);
};

load();
