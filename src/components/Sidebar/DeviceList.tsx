import { useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useStore } from '../../store';
import { scanProgressDetail, scanProgressText } from '../../utils/scanText';
import styles from './DeviceList.module.css';

const STATUS_LABEL: Record<string, string> = {
  online: 'Online',
  offline: 'Offline',
  unauthorized: 'Auth?',
  connecting: '...',
};

export function DeviceList() {
  const devices = useStore((s) => s.devices);
  const selectedSerials = useStore((s) => s.selectedSerials);
  const disabledSerials = useStore((s) => s.disabledSerials);
  const toggleSelect = useStore((s) => s.toggleSelect);
  const toggleDisableDevice = useStore((s) => s.toggleDisableDevice);
  const selectAll = useStore((s) => s.selectAll);
  const clearSelection = useStore((s) => s.clearSelection);
  const scanProgress = useStore((s) => s.scanProgress);
  const [filter, setFilter] = useState('');
  const [refreshing, setRefreshing] = useState(false);
  const [lastRefresh, setLastRefresh] = useState<Date | null>(null);

  // The command returns as soon as the sweep is spawned, so the button stays
  // disabled off the sweep's own progress events, not off the await.
  const scanning = !!scanProgress && !scanProgress.done;
  const scanText = scanProgressText(scanProgress);

  const selectedCount = selectedSerials.size;
  const keyword = filter.trim().toLowerCase();
  const filtered = keyword
    ? devices.filter((d) =>
        d.serial.toLowerCase().includes(keyword) ||
        d.model.toLowerCase().includes(keyword)
      )
    : devices;

  function launchScrcpy(d: typeof devices[0]) {
    invoke('launch_scrcpy', {
      serial: d.serial,
      serverHost: d.server_host,
      serverPort: d.server_port,
    }).catch(() => {});
  }

  async function handleRefresh() {
    if (refreshing || scanning) return;
    setRefreshing(true);
    try {
      // Also sweeps the segments configured on the server entries and attaches
      // whatever answers, so wireless devices show up with this refresh.
      await invoke('refresh_devices', { scan: true });
      setLastRefresh(new Date());
    } finally {
      setRefreshing(false);
    }
  }

  function formatLastRefresh(d: Date) {
    return d.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit' });
  }

  return (
    <div className={styles.wrap}>
      <div className={styles.header}>
        <span className={styles.title}>
          Devices <span className={styles.count}>{devices.length}</span>
        </span>
        <div className={styles.actions}>
          <button className={styles.actionBtn} onClick={selectAll}>All</button>
          <button className={styles.actionBtn} onClick={clearSelection}>None</button>
          <button
            className={`${styles.actionBtn} ${refreshing || scanning ? styles.refreshing : ''}`}
            onClick={handleRefresh}
            title="Refresh device list (also sweeps segments set on the servers)"
            disabled={refreshing || scanning}
          >
            ↻
          </button>
        </div>
      </div>

      {lastRefresh && (
        <div className={styles.lastRefresh}>Updated {formatLastRefresh(lastRefresh)}</div>
      )}

      {scanText && (
        <div
          className={scanning ? styles.scanning : styles.lastRefresh}
          title={scanProgressDetail(scanProgress)}
        >
          {scanText}
        </div>
      )}

      <input
        className={styles.filterInput}
        placeholder="Filter by ID..."
        value={filter}
        onChange={(e) => setFilter(e.target.value)}
      />

      {selectedCount > 0 && (
        <div className={styles.selInfo}>{selectedCount} selected</div>
      )}

      <div className={styles.list}>
        {filtered.length === 0 ? (
          <div className={styles.empty}>
            {devices.length === 0 ? 'No devices detected' : 'No match'}
          </div>
        ) : (
          filtered.map((d) => (
            <div
              key={d.serial}
              className={`${styles.item} ${selectedSerials.has(d.serial) ? styles.selected : ''} ${disabledSerials.has(d.serial) ? styles.disabled : ''}`}
              onClick={() => d.status === 'online' && !disabledSerials.has(d.serial) && toggleSelect(d.serial)}
            >
              <div
                className={`${styles.statusDot} ${styles[`status_${d.status}`]}`}
                onClick={(e) => { e.stopPropagation(); toggleDisableDevice(d.serial); }}
                title={disabledSerials.has(d.serial) ? 'Click to enable' : 'Click to disable'}
                style={{ cursor: 'pointer' }}
              />
              <div className={styles.info}>
                <div className={styles.name}>{d.serial}</div>
                <div className={styles.meta}>
                  {disabledSerials.has(d.serial) ? 'Disabled' : STATUS_LABEL[d.status]}
                </div>
              </div>
              {d.status === 'online' && !disabledSerials.has(d.serial) && (
                <button
                  className={styles.scrcpyBtn}
                  title="Open in scrcpy"
                  onClick={(e) => { e.stopPropagation(); launchScrcpy(d); }}
                >
                  ▶
                </button>
              )}
            </div>
          ))
        )}
      </div>
    </div>
  );
}
