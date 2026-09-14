/** Keep native revisions ordered while letting queued interactions run first. */
export class LiveQueue {
  private interactive: Array<() => Promise<void>> = [];
  private background: Array<() => Promise<void>> = [];
  private running = false;

  enqueue<T>(operation: () => Promise<T>, interactive = false): Promise<T> {
    const result = new Promise<T>((resolve, reject) => {
      const jobs = interactive ? this.interactive : this.background;
      jobs.push(async () => {
        try { resolve(await operation()); }
        catch (error) { reject(error); }
      });
    });
    if (!this.running) void this.drain();
    return result;
  }

  private async drain(): Promise<void> {
    this.running = true;
    try {
      let job: (() => Promise<void>) | undefined;
      while ((job = this.interactive.shift() || this.background.shift())) await job();
    } finally {
      this.running = false;
    }
  }
}
