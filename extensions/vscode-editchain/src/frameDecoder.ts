/** Length-prefixed frames, assembled without recopying each received prefix. */
export class FrameDecoder {
  private header = Buffer.alloc(4);
  private headerBytes = 0;
  private payload: Buffer | null = null;
  private payloadBytes = 0;

  *push(chunk: Buffer): Generator<Buffer> {
    let offset = 0;
    while (offset < chunk.length) {
      if (this.payload === null) {
        const length = Math.min(4 - this.headerBytes, chunk.length - offset);
        chunk.copy(this.header, this.headerBytes, offset, offset + length);
        this.headerBytes += length;
        offset += length;
        if (this.headerBytes < 4) break;
        this.payload = Buffer.allocUnsafe(this.header.readUInt32LE(0));
      }
      const length = Math.min(this.payload.length - this.payloadBytes, chunk.length - offset);
      chunk.copy(this.payload, this.payloadBytes, offset, offset + length);
      this.payloadBytes += length;
      offset += length;
      if (this.payloadBytes < this.payload.length) break;
      const complete = this.payload;
      this.payload = null;
      this.payloadBytes = 0;
      this.headerBytes = 0;
      yield complete;
    }
  }
}
