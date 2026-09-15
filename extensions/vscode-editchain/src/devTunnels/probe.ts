import { randomBytes } from 'node:crypto';
import { performance } from 'node:perf_hooks';
import { Duplex } from 'node:stream';
import { CancellationToken } from 'vscode-jsonrpc';

export class ProbeError extends Error {}

/** Consume only the expected, bounded bytes; never log bytes supplied by a peer. */
export function receiveExpected(
  stream: Duplex, expected: Buffer, cancellation: CancellationToken
): Promise<void> {
  return new Promise((resolve, reject) => {
    let offset = 0;
    const finish = (error?: Error) => {
      stream.pause();
      stream.off('data', data);
      stream.off('error', failed);
      stream.off('end', closed);
      stream.off('close', closed);
      subscription.dispose();
      if (error) reject(error); else resolve();
    };
    const failed = () => finish(new ProbeError('Probe stream failed.'));
    const closed = () => finish(new ProbeError('Probe stream closed before all bytes arrived.'));
    const data = (chunk: Buffer) => {
      if (!Buffer.isBuffer(chunk) || !chunk.equals(expected.subarray(offset, offset + chunk.length))) {
        finish(new ProbeError('Probe payload integrity check failed.'));
        return;
      }
      offset += chunk.length;
      if (offset === expected.length) finish();
    };
    const subscription = cancellation.onCancellationRequested(() =>
      finish(new ProbeError('Probe cancelled or timed out.')));
    stream.on('data', data);
    stream.on('error', failed);
    stream.on('end', closed);
    stream.on('close', closed);
    if (cancellation.isCancellationRequested) finish(new ProbeError('Probe cancelled or timed out.'));
    else if (stream.destroyed || stream.readableEnded) closed();
    else stream.resume();
  });
}

function send(stream: Duplex, payload: Buffer): Promise<void> {
  return new Promise((resolve, reject) => {
    stream.write(payload, error => {
      if (error) reject(new ProbeError('Probe write failed.')); else resolve();
    });
  });
}

export type ProbeMetrics = {
  bytesEachDirection: number;
  roundTrips: number;
  rttMs: { min: number; p50: number; p95: number; max: number };
};

/** Both ends use real relay streams. All payloads are synthetic and memory-only. */
export async function probeStreams(
  host: Duplex, client: Duplex, cancellation: CancellationToken
): Promise<ProbeMetrics> {
  const clientPayload = randomBytes(16 * 1024);
  const hostPayload = randomBytes(16 * 1024);
  await Promise.all([
    receiveExpected(host, clientPayload, cancellation),
    receiveExpected(client, hostPayload, cancellation),
    send(host, hostPayload),
    send(client, clientPayload),
  ]);

  const samples: number[] = [];
  for (let index = 0; index < 20; index++) {
    const payload = randomBytes(32);
    const start = performance.now();
    await Promise.all([
      receiveExpected(client, payload, cancellation),
      receiveExpected(host, payload, cancellation).then(() => send(host, payload)),
      send(client, payload),
    ]);
    samples.push(performance.now() - start);
  }
  samples.sort((a, b) => a - b);
  const at = (index: number) => Math.round(samples[index] * 100) / 100;
  return {
    bytesEachDirection: clientPayload.length + samples.length * 32,
    roundTrips: samples.length,
    rttMs: { min: at(0), p50: at(9), p95: at(18), max: at(19) },
  };
}
