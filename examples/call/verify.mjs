// External verification tooling only: install Playwright separately, then set
// PLAYWRIGHT_MODULE to its index.mjs when it isn't resolvable from this folder.
import assert from 'node:assert/strict';
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || 'playwright');
const base = process.env.CALL_BASE_URL || 'http://127.0.0.1:18088';
const key = process.env.CALL_TOKEN;
assert(key, 'CALL_TOKEN must match the local test server');
const browser = await chromium.launch({headless:true,args:[
  '--use-fake-ui-for-media-stream','--use-fake-device-for-media-stream','--autoplay-policy=no-user-gesture-required',
]});
const errors = [];
const contexts = [];
try {
  async function tab() {
    const context = await browser.newContext({permissions:['camera','microphone']});
    contexts.push(context);
    await context.addInitScript(() => {
      window.__peers = []; window.__sockets = []; window.__messages = [];
      const PC = RTCPeerConnection, WS = WebSocket;
      window.RTCPeerConnection = class extends PC { constructor(...args) {super(...args); window.__peers.push(this);} };
      window.WebSocket = class extends WS { constructor(...args) {super(...args); window.__sockets.push(this); this.addEventListener('message',e => window.__messages.push(JSON.parse(e.data)));} };
    });
    const page = await context.newPage();
    page.on('pageerror',e => errors.push(e.message));
    await page.goto(base);
    return page;
  }
  const a = await tab(), b = await tab(), guest = await tab();
  assert.equal((await guest.request.post(`${base}/session`,{headers:{origin:base},data:{room:'test',key:'wrong'}})).status(),403);
  assert.equal((await guest.request.post(`${base}/session`,{headers:{origin:'https://untrusted.invalid'},data:{room:'test',key}})).status(),403);
  const unauthenticated = await guest.evaluate(() => new Promise(resolve => {
    const ws = new WebSocket(`ws://${location.host}/signal`); ws.onopen = () => resolve(false); ws.onclose = () => resolve(true);
  }));
  assert(unauthenticated);
  async function join(page,room='test') {
    await page.locator('#room').fill(room); await page.locator('#key').fill(key); await page.locator('#join').click();
  }
  async function connected(page) {
    await page.waitForFunction(() => window.__peers.at(-1)?.connectionState === 'connected',null,{timeout:25000});
    const deadline = Date.now() + 15000;
    while(true) {
      const receiving = await page.evaluate(async () => {
        const pc = window.__peers.at(-1); if(!pc || pc.connectionState !== 'connected') return false;
        const stats = [...(await pc.getStats()).values()];
        return stats.some(s=>s.type==='inbound-rtp' && s.kind==='video' && s.framesDecoded>=5)
          && stats.some(s=>s.type==='inbound-rtp' && s.kind==='audio' && s.bytesReceived>100);
      });
      if(receiving) break;
      assert(Date.now()<deadline, 'media did not arrive');
      await new Promise(resolve=>setTimeout(resolve,100));
    }
  }
  await join(a); await join(b); await Promise.all([connected(a),connected(b)]);
  const media = [];
  for(const page of [a,b]) {
    media.push(await page.evaluate(async () => {
      const stats = await window.__peers.at(-1).getStats();
      const transport = [...stats.values()].find(s=>s.type==='transport' && s.selectedCandidatePairId);
      const pair = stats.get(transport.selectedCandidatePairId);
      return {local:stats.get(pair.localCandidateId).candidateType,remote:stats.get(pair.remoteCandidateId).candidateType,
        videoFrames:[...stats.values()].filter(s=>s.type==='inbound-rtp' && s.kind==='video').reduce((n,s)=>n+(s.framesDecoded||0),0),
        audioBytes:[...stats.values()].filter(s=>s.type==='inbound-rtp' && s.kind==='audio').reduce((n,s)=>n+(s.bytesReceived||0),0)};
    }));
  }
  if(process.env.EXPECT_RELAY === '1') assert(media.every(m=>m.local==='relay' && m.remote==='relay'),JSON.stringify(media));
  assert(media.every(m=>m.videoFrames>0 && m.audioBytes>0),JSON.stringify(media));
  console.log('media',JSON.stringify(media));
  const priorCounts = await Promise.all([a,b].map(p=>p.evaluate(()=>window.__peers.length)));
  await a.evaluate(()=>window.__sockets.at(-1).close(4000,'test interruption'));
  for(const [i,page] of [a,b].entries()) await page.waitForFunction(n=>window.__peers.length>n,priorCounts[i]);
  await Promise.all([connected(a),connected(b)]);
  // A stale connection generation must not relay an invalid SDP to the peer.
  await a.evaluate(() => window.__sockets.at(-1).send(JSON.stringify({type:'signal',generation:window.__messages.find(m=>m.type==='ready').generation,payload:{description:{type:'offer',sdp:'stale'}}})));
  console.log('reconnect and stale-generation fencing passed');
  await a.locator('#record').click();
  await a.waitForTimeout(2500);
  await a.locator('#stop-record').click();
  await a.waitForFunction(()=>document.getElementById('saved').textContent.startsWith('Recording saved'),null,{timeout:15000});
  const path = await a.locator('#playback').getAttribute('src');
  const head = await a.request.head(`${base}${path}`);
  assert.equal(head.status(),200); const length = Number(head.headers()['content-length']); assert(length>1000);
  const range = await a.request.get(`${base}${path}`,{headers:{range:'bytes=2-9'}});
  assert.equal(range.status(),206); assert.equal((await range.body()).length,8); assert.equal(range.headers()['content-range'],`bytes 2-9/${length}`);
  const unsatisfiable = await a.request.get(`${base}${path}`,{headers:{range:`bytes=${length}-`}});
  assert.equal(unsatisfiable.status(),416);
  assert.equal((await guest.request.get(`${base}${path}`)).status(),403);
  await guest.request.post(`${base}/session`,{headers:{origin:base},data:{room:'other-room',key}});
  assert.equal((await guest.request.get(`${base}${path}`)).status(),404);
  await a.evaluate(async () => {const video = document.getElementById('playback'); await video.play(); video.currentTime = 0.5;});
  await a.waitForFunction(()=>document.getElementById('playback').currentTime>=0.5 && document.getElementById('playback').readyState>=2);
  await a.screenshot({path:process.env.CALL_SCREENSHOT || '/tmp/sutegi-call-browser.png',fullPage:true});
  console.log('recording upload, playback, seek, HEAD, 206, 416, and room authorization passed',length);
  await a.locator('#hangup').click();
  await b.waitForFunction(()=>document.getElementById('status').textContent==='Waiting for the other participant');
  assert.deepEqual(errors,[]);
  console.log('PASS',process.env.EXPECT_RELAY==='1'?'TURN relay':'direct');
} finally {
  await Promise.all(contexts.map(c=>c.close())); await browser.close();
}
