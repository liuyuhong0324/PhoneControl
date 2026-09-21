import { useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useStore } from '../../store';
import type { AdbServer } from '../../types';
import { isLocalServer, LOCAL_ADB_PORT, LOCAL_HOST } from '../../utils/servers';
import styles from './ServerList.module.css';

export function ServerList() {
  const servers = useStore((s) => s.servers);
  const setServers = useStore((s) => s.setServers);
  const [host, setHost] = useState('');
  const [error, setError] = useState('');

  // The local daemon is not something the user added, and taking it away would
  // leave the app with nothing to talk to — so it is never removed. It is shown
  // because it is what the devices are reached through.
  const added = servers.filter((s) => !isLocalServer(s));

  async function addServer() {
    const typed = host.trim();
    if (!typed) return;
    // The address is the network the refresh sweeps: `192.168.101.1` means the
    // /24 it sits in, and a phone found there is connected through the local
    // daemon. Nothing else to ask for — no port, no second field.
    try {
      const srv = await invoke<AdbServer>('add_server', { host: typed, port: LOCAL_ADB_PORT });
      setServers([...servers, srv]);
      setHost('');
      setError('');
      invoke('refresh_devices').catch(() => {});
    } catch (e: any) {
      setError(String(e));
    }
  }

  async function removeServer(id: string) {
    await invoke('remove_server', { id });
    setServers(servers.filter((s) => s.id !== id));
  }

  async function toggleServer(id: string, enabled: boolean) {
    await invoke('toggle_server', { id, enabled });
    setServers(servers.map((s) => (s.id === id ? { ...s, enabled } : s)));
    // 切换 server 状态后自动刷新设备列表
    invoke('refresh_devices').catch(() => {});
  }

  return (
    <div className={styles.wrap}>
      <div className={styles.title}>ADB Servers</div>

      <div className={styles.addRow}>
        <input
          className={styles.input}
          placeholder="192.168.101.1"
          value={host}
          onChange={(e) => setHost(e.target.value)}
          onKeyDown={(e) => e.key === 'Enter' && addServer()}
          title="The network swept when you refresh: one address (192.168.101.1) means the /24 it sits in; 192.168.1.0/24 and 192.168.1.5-192.168.1.40 work too. Devices found are connected through the local adb server (127.0.0.1:5037)."
        />
        <button className={styles.addBtn} onClick={addServer}>+</button>
      </div>

      {error && <div className={styles.error}>{error}</div>}

      <div className={styles.list}>
        {/* The row the app always has, in the shape of the rows the user adds. */}
        <div className={styles.item}>
          <div
            className={`${styles.dot} ${styles.dotOn}`}
            title="Always on: the adb daemon on this machine (127.0.0.1:5037) that the USB devices are reached through"
          />
          <span className={styles.addr}>{LOCAL_HOST}:{LOCAL_ADB_PORT}</span>
        </div>
        {added.map((srv) => (
          <div key={srv.id} className={styles.item}>
            <div
              className={`${styles.dot} ${srv.enabled ? styles.dotOn : styles.dotOff}`}
              onClick={() => toggleServer(srv.id, !srv.enabled)}
              title={srv.enabled ? 'Click to disable' : 'Click to enable'}
            />
            <span className={styles.addr}>{srv.host}</span>
            <button className={styles.removeBtn} onClick={() => removeServer(srv.id)}>×</button>
          </div>
        ))}
      </div>
    </div>
  );
}
