const $ = id => document.getElementById(id);
const status = message => { $('status').textContent = message; };
let media, socket, peer, config, generation, candidates = [], reconnectTimer, recordTimer;
let active = false, reconnectAttempt = 0, recorder, chunks = [], recordingBytes = 0, saving = false;
let events = Promise.resolve();
function resetPeer() {
  if (peer) { peer.close(); peer = undefined; }
  candidates = []; generation = undefined; $('remote').srcObject = null;
}
function signal(payload, expected) {
  if (active && expected === generation && socket?.readyState === WebSocket.OPEN) {
    socket.send(JSON.stringify({ type: 'signal', generation, payload }));
  }
}
function fail(error) { status(error.message || String(error)); }
function makePeer(nextGeneration) {
  resetPeer(); generation = nextGeneration;
  const current = peer = new RTCPeerConnection(config);
  for (const track of media.getTracks()) current.addTrack(track, media);
  current.onicecandidate = ({candidate}) => { if(candidate) signal({candidate: candidate.toJSON()}, nextGeneration); };
  current.ontrack = event => { if(peer === current) $('remote').srcObject = event.streams[0]; };
  current.onconnectionstatechange = () => {
    if (peer !== current) return;
    status(`Call ${current.connectionState}`);
    if (current.connectionState === 'failed') socket?.close(4000, 'restart signalling');
  };
  return current;
}
async function receive(message) {
  if (!active) return;
  if (message.type === 'waiting' || message.type === 'peer-left') {
    resetPeer(); status('Waiting for the other participant'); return;
  }
  if (message.type === 'ready') {
    const current = makePeer(message.generation);
    status('Connecting media');
    if (message.initiator) {
      await current.setLocalDescription(await current.createOffer());
      if (peer === current) signal({description: current.localDescription}, message.generation);
    }
    return;
  }
  if (message.type !== 'signal' || message.generation !== generation || !peer) return;
  const current = peer, expected = generation, {description, candidate} = message.payload;
  if (description) {
    if (!['offer', 'answer'].includes(description.type)) throw new Error('Invalid session description');
    await current.setRemoteDescription(description);
    if (peer !== current) return;
    for (const pending of candidates) await current.addIceCandidate(pending);
    candidates = [];
    if (description.type === 'offer') {
      await current.setLocalDescription(await current.createAnswer());
      if (peer === current) signal({description: current.localDescription}, expected);
    }
  } else if (candidate) {
    if (current.remoteDescription) await current.addIceCandidate(candidate);
    else if(candidates.length < 128) candidates.push(candidate);
    else throw new Error('Too many ICE candidates');
  }
}
function connect() {
  if (!active) return;
  const current = socket = new WebSocket(`${location.protocol === 'https:' ? 'wss:' : 'ws:'}//${location.host}/signal`);
  current.onopen = () => { if (socket === current) { reconnectAttempt = 0; status('Signalling connected'); } };
  current.onmessage = event => {
    // Serialize SDP/ICE operations; an old socket cannot mutate a new peer.
    events = events.then(() => { if(socket === current) return receive(JSON.parse(event.data)); })
      .catch(error => { if(socket === current) { fail(error); current.close(4000,'invalid signalling'); } });
  };
  current.onclose = event => {
    if(socket !== current || !active) return;
    resetPeer();
    if([1000,1008].includes(event.code)) { leave(); status(event.reason || 'Session ended; join again'); return; }
    if(++reconnectAttempt > 8) { leave(); status('Could not reconnect. Join again when the server is ready.'); return; }
    const delay = Math.min(500 * 2 ** (reconnectAttempt - 1), 8000) + Math.random()*250;
    status('Signalling interrupted; reconnecting');
    reconnectTimer = setTimeout(connect, delay);
  };
}
function leave() {
  active = false; clearTimeout(reconnectTimer); clearTimeout(recordTimer);
  socket?.close(1000,'left'); socket = undefined; resetPeer();
  if(recorder?.state === 'recording') recorder.stop();
  media?.getTracks().forEach(track => track.stop()); media = undefined; $('local').srcObject = null;
  $('join').disabled = false; $('hangup').disabled = true; $('record').disabled = true; $('stop-record').disabled = true;
  status('Call ended');
}
$('join-form').addEventListener('submit', async event => {
  event.preventDefault(); $('join').disabled = true;
  try {
    const response = await fetch('/session', { method:'POST', headers:{'content-type':'application/json'},
      body:JSON.stringify({room:$('room').value, key:$('key').value}) });
    if(!response.ok) throw new Error(await response.text());
    config = await response.json(); $('key').value = '';
    media = await navigator.mediaDevices.getUserMedia({audio:true,video:true});
    $('local').srcObject = media; active = true; reconnectAttempt = 0;
    $('hangup').disabled = false;
    $('record').disabled = !globalThis.MediaRecorder?.isTypeSupported('video/webm');
    connect();
  } catch(error) { leave(); fail(error); }
});
$('hangup').addEventListener('click',leave);
window.addEventListener('pagehide',leave);
$('record').addEventListener('click', () => {
  if(!media || saving) return;
  chunks = []; recordingBytes = 0;
  recorder = new MediaRecorder(media, {mimeType:'video/webm'});
  recorder.ondataavailable = ({data}) => {
    chunks.push(data); recordingBytes += data.size;
    if(recordingBytes >= 48*1024*1024 && recorder.state === 'recording') recorder.stop();
  };
  recorder.onerror = event => { fail(event.error); };
  recorder.onstop = async () => {
    clearTimeout(recordTimer); saving = true;
    $('stop-record').disabled = true; $('saved').textContent = 'Saving recording…';
    try {
      const blob = new Blob(chunks,{type:'video/webm'}); chunks = [];
      if(blob.size > 64*1024*1024) throw new Error('Recording exceeded the 64 MiB upload limit');
      const response = await fetch('/recordings',{method:'POST',headers:{'content-type':'video/webm'},body:blob});
      if(!response.ok) throw new Error(await response.text());
      const {url} = await response.json();
      $('playback').src = url; $('playback').hidden = false;
      $('saved').textContent = 'Recording saved. Play or seek below.';
    } catch(error) { $('saved').textContent = `Could not save: ${error.message}`; }
    finally { saving = false; $('record').disabled = !active; }
  };
  recorder.start(1000); recordTimer = setTimeout(() => { if(recorder.state === 'recording') recorder.stop(); },60000);
  $('record').disabled = true; $('stop-record').disabled = false; $('saved').textContent = 'Recording your camera and microphone';
});
$('stop-record').addEventListener('click', () => { if(recorder?.state === 'recording') recorder.stop(); });
