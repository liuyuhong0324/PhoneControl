import type { AdbServer } from '../types';

/** The daemon this app starts and keeps for the USB devices. */
export const LOCAL_HOST = '127.0.0.1';
export const LOCAL_ADB_PORT = 5037;

/**
 * The local daemon is not a server the user added: it is always there, and
 * taking it away would leave the app with nothing to talk to. Recognised by host
 * and port, so a remote server that happens to use 5037 is still the user's.
 */
export function isLocalServer(srv: AdbServer): boolean {
  return (
    (srv.host === LOCAL_HOST || srv.host === 'localhost') && srv.port === LOCAL_ADB_PORT
  );
}
