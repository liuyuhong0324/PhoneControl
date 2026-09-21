import type { ScanProgress } from '../types';

/**
 * One line describing a sweep for the segment under the refresh button — the
 * backend sends progress events while it runs, then a final one with `done`.
 * Returns null when there is nothing worth showing.
 */
export function scanProgressText(progress: ScanProgress | null): string | null {
  if (!progress) return null;

  if (progress.done) {
    // A segment that could not even be read: there is nothing to report but
    // the reason.
    if (progress.total === 0) {
      if (progress.error) return `Scan failed: ${progress.error}`;
      // Nothing was probed because everything in the segment is already
      // attached — "0 found" would read as a failure, so say what is there.
      return progress.alreadyConnected
        ? `${progress.alreadyConnected} already connected · nothing new`
        : null;
    }
    const summary = `Scan: ${progress.found.length} found, ${progress.connected} attached`;
    return progress.alreadyConnected
      ? `${summary} · ${progress.alreadyConnected} already connected`
      : summary;
  }

  return `Scanning ${progress.segment} ${progress.scanned}/${progress.total} · found ${progress.found.length}`;
}

/**
 * Tooltip for that line: the addresses that answered, plus anything that went
 * wrong attaching them (a device can answer on 5555 and still refuse to be an
 * adb device).
 */
export function scanProgressDetail(progress: ScanProgress | null): string | undefined {
  if (!progress) return undefined;

  const lines = progress.found.length ? [...progress.found] : [];
  if (progress.done) {
    if (!lines.length && progress.total > 0) lines.push('no new devices answered');
    if (progress.alreadyConnected) {
      lines.push(`${progress.alreadyConnected} already connected, not probed`);
    }
  }
  if (progress.error) lines.push(progress.error);
  return lines.length ? lines.join('\n') : undefined;
}
