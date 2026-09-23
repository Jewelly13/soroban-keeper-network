/**
 * Bounded concurrency control via a simple semaphore/permit pattern.
 *
 * This is the core mechanism for enforcing maxConcurrentTasks.
 * At most N tasks can be in flight simultaneously; excess tasks wait.
 */

/**
 * A semaphore that permits up to `limit` concurrent acquisitions.
 * Excess callers await until permits are released.
 */
export class Semaphore {
  private permits: number;
  private waiters: Array<() => void> = [];

  constructor(limit: number) {
    if (limit < 1) {
      throw new Error("Semaphore limit must be >= 1");
    }
    this.permits = limit;
  }

  /**
   * Acquire a permit. If none available, waits until one is released.
   * Returns a release function that must be called when done.
   */
  async acquire(): Promise<() => void> {
    if (this.permits > 0) {
      this.permits--;
      // Return the release function immediately
      return () => this.release();
    }

    // No permits available; wait
    return new Promise((resolve) => {
      this.waiters.push(() => {
        this.permits--;
        resolve(() => this.release());
      });
    });
  }

  /**
   * Release a permit and wake the next waiter, if any.
   */
  private release(): void {
    if (this.waiters.length > 0) {
      const waiter = this.waiters.shift();
      if (waiter) {
        waiter();
      }
    } else {
      this.permits++;
    }
  }

  /**
   * Get current number of available permits.
   * Useful for monitoring/debugging.
   */
  available(): number {
    return this.permits;
  }

  /**
   * Get number of tasks currently waiting for a permit.
   */
  waiting(): number {
    return this.waiters.length;
  }
}

/**
 * A convenience function that runs fn concurrently for each item in items,
 * respecting the semaphore limit.
 *
 * Returns an array of results in the same order as items.
 * If any fn throws, the error is stored in that position in the results array.
 */
export async function runConcurrentWithLimit<T, R>(
  items: T[],
  concurrencyLimit: number,
  fn: (item: T) => Promise<R>
): Promise<Array<R | Error>> {
  const semaphore = new Semaphore(concurrencyLimit);
  const results: Array<R | Error> = [];

  const promises = items.map(async (item, index) => {
    const release = await semaphore.acquire();
    try {
      const result = await fn(item);
      results[index] = result;
    } catch (error) {
      results[index] = error instanceof Error ? error : new Error(String(error));
    } finally {
      release();
    }
  });

  await Promise.all(promises);
  return results;
}
