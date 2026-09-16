// Reproduces the app's start_scrcpy_and_connect flow for all devices.
const { execFile, spawn } = require('child_process');
const net = require('net');
const fs = require('fs');

const ADB = './scrcpy/adb.exe';
const SERVER_LOCAL = './scrcpy/scrcpy-server';
const SERVER_SIZE = fs.statSync(SERVER_LOCAL).size;
const REMOTE = '/data/local/tmp/scrcpy-server.jar';
const VER = '3.3.4';
const CONCURRENCY = parseInt(process.argv[2] || '12', 10);

const adb = (args, timeoutMs = 15000) => new Promise((resolve, reject) => {
  const t = setTimeout(() => reject(new Error('timeout')), timeoutMs);
  execFile(ADB, args, { windowsHide: true }, (err, stdout, stderr) => {
    clearTimeout(t);
    if (err) reject(new Error(`${err.message} | ${stderr}`));
    else resolve(stdout);
  });
});

const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

async function connectDevice(serial) {
  const t0 = Date.now();
  const log = (msg) => console.log(`[${serial}] (+${Date.now() - t0}ms) ${msg}`);

  // 1) size check
  const sizeOut = await adb(['-s', serial, 'shell',
    `stat -c %s ${REMOTE} 2>/dev/null || wc -c < ${REMOTE} 2>/dev/null || echo 0`], 15000).catch(() => '0');
  const remoteSize = parseInt((sizeOut.match(/\d+/) || ['0'])[0], 10);
  if (remoteSize !== SERVER_SIZE) {
    log(`push server (remote=${remoteSize} local=${SERVER_SIZE})`);
    await adb(['-s', serial, 'push', SERVER_LOCAL, REMOTE], 60000);
  } else {
    log('server already on device');
  }

  // 2) start server
  const scid = Math.floor(Math.random() * 0x7fffffff);
  const argv = ['-s', serial, 'shell',
    `CLASSPATH=${REMOTE}`, 'app_process', '/', 'com.genymobile.scrcpy.Server', VER,
    `scid=${scid.toString(16).padStart(8, '0')}`,
    'log_level=info', 'audio=false', 'control=false', 'tunnel_forward=true',
    'stay_awake=true', 'max_size=720', 'max_fps=30', 'video_bit_rate=4000000',
    'video_codec_options=i-frame-interval=1', 'send_device_meta=false',
    'send_codec_meta=true', 'send_dummy_byte=true', 'send_frame_meta=true'];
  const child = spawn(ADB, argv, { windowsHide: true });
  let serverLog = '';
  child.stdout.on('data', (d) => { serverLog += d.toString(); });
  child.stderr.on('data', (d) => { serverLog += d.toString(); });

  // 3) forward tcp:0
  const fwdOut = await adb(['-s', serial, 'forward', 'tcp:0', `localabstract:scrcpy_${scid.toString(16).padStart(8, '0')}`], 15000);
  const port = parseInt(fwdOut.trim(), 10);
  if (!port) throw new Error(`forward failed: ${fwdOut}`);
  log(`forward port=${port}`);

  // 4) TCP connect with retry (3s)
  const deadline = Date.now() + 3000;
  let lastErr = 'none';
  let ok = false;
  while (Date.now() < deadline) {
    try {
      ok = await new Promise((resolve) => {
        const s = net.connect({ host: '127.0.0.1', port }, () => {
          s.setTimeout(300);
          s.once('data', () => { s.destroy(); resolve(true); });
          s.once('timeout', () => { s.destroy(); resolve(true); }); // alive, no data yet
          s.once('error', () => resolve(false));
          s.once('close', (hadErr) => { resolve(false); });
          s.once('end', () => resolve(false));
        });
        s.once('error', (e) => { lastErr = e.message; resolve(false); });
      });
      if (ok) break;
    } catch (e) { lastErr = e.message; }
    await sleep(40);
  }

  if (ok) {
    log('CONNECTED, got data (or alive socket)');
  } else {
    log(`FAILED: ${lastErr}`);
    console.log(`[${serial}] server log:\n${serverLog.slice(0, 1500)}`);
  }

  child.kill();
  await adb(['-s', serial, 'forward', '--remove', `tcp:${port}`], 5000).catch(() => {});
  return ok;
}

(async () => {
  const listOut = await adb(['devices']);
  const serials = listOut.split('\n').slice(1)
    .map((l) => l.trim().split('\t'))
    .filter(([s, st]) => st === 'device')
    .map(([s]) => s);
  console.log(`devices=${serials.length} concurrency=${CONCURRENCY}`);
  const t0 = Date.now();
  const results = {};
  let idx = 0;
  const workers = Array.from({ length: Math.min(CONCURRENCY, serials.length) }, async () => {
    while (idx < serials.length) {
      const serial = serials[idx++];
      try { results[serial] = await connectDevice(serial); }
      catch (e) { console.log(`[${serial}] ERROR: ${e.message}`); results[serial] = false; }
    }
  });
  await Promise.all(workers);
  const okCount = Object.values(results).filter(Boolean).length;
  console.log(`\n=== ${okCount}/${serials.length} connected in ${Date.now() - t0}ms ===`);
})();
