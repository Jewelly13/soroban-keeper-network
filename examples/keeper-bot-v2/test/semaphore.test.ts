/**
 * Tests for the semaphore/concurrency control (utility for worker pool pattern)
 */

import { describe, it, expect } from "vitest";
import { Semaphore, runConcurrentWithLimit } from "../src/semaphore.js";

describe("Semaphore", () => {
  describe("construction and initial state", () => {
    it("initializes with the specified limit", () => {
      const sem = new Semaphore(3);
      expect(sem.available()).toBe(3);
      expect(sem.waiting()).toBe(0);
    });

    it("rejects limit < 1", () => {
      expect(() => new Semaphore(0)).toThrow();
      expect(() => new Semaphore(-1)).toThrow();
    });
  });

  describe("acquire and release", () => {
    it("allows immediate acquisition when permits available", async () => {
      const sem = new Semaphore(2);
      const r1 = await sem.acquire();
      expect(sem.available()).toBe(1);
      r1();
      expect(sem.available()).toBe(2);
    });

    it("blocks when no permits available", (done) => {
      const sem = new Semaphore(1);
      let blocked = true;

      (async () => {
        const r1 = await sem.acquire();
        expect(blocked).toBe(true);
        r1();

        const r2 = await sem.acquire();
        expect(blocked).toBe(false);
        r2();
        done();
      })();

      (async () => {
        // Hold the permit for a bit
        await new Promise((r) => setTimeout(r, 50));
        blocked = false;
      })();
    });

    it("preserves order of waiters (FIFO)", async () => {
      const sem = new Semaphore(1);
      const order: number[] = [];

      // Acquire the only permit
      const release0 = await sem.acquire();

      // Queue 3 waiters
      const p1 = sem.acquire().then((r) => {
        order.push(1);
        r();
      });
      const p2 = sem.acquire().then((r) => {
        order.push(2);
        r();
      });
      const p3 = sem.acquire().then((r) => {
        order.push(3);
        r();
      });

      // Release triggers waiter 1
      release0();
      await p1;
      // At this point, waiter 2 should have the permit
      await p2;
      // Then waiter 3
      await p3;

      expect(order).toEqual([1, 2, 3]);
    });
  });

  describe("multiple concurrent acquisitions", () => {
    it("allows N tasks to run concurrently up to the limit", async () => {
      const sem = new Semaphore(3);
      let concurrent = 0;
      let maxConcurrent = 0;

      const worker = async () => {
        const release = await sem.acquire();
        concurrent++;
        maxConcurrent = Math.max(maxConcurrent, concurrent);
        await new Promise((r) => setTimeout(r, 10));
        concurrent--;
        release();
      };

      await Promise.all([
        worker(),
        worker(),
        worker(),
        worker(),
        worker(),
        worker(),
      ]);

      expect(maxConcurrent).toBe(3);
    });

    it("tracks waiters correctly", async () => {
      const sem = new Semaphore(1);
      const r0 = await sem.acquire();
      expect(sem.waiting()).toBe(0);

      const p1 = sem.acquire();
      expect(sem.waiting()).toBe(1);

      const p2 = sem.acquire();
      expect(sem.waiting()).toBe(2);

      r0();
      await p1;
      expect(sem.waiting()).toBe(1);

      const r1 = await sem.acquire();
      expect(sem.waiting()).toBe(0);
      r1();
    });
  });
});

describe("runConcurrentWithLimit", () => {
  it("processes all items", async () => {
    const items = [1, 2, 3];
    const results = await runConcurrentWithLimit(items, 2, async (x) => x * 2);
    expect(results).toEqual([2, 4, 6]);
  });

  it("respects concurrency limit strictly", async () => {
    const items = Array.from({ length: 20 }, (_, i) => i);
    const limit = 3;
    let maxConcurrent = 0;
    let currentConcurrent = 0;

    await runConcurrentWithLimit(items, limit, async (item) => {
      currentConcurrent++;
      maxConcurrent = Math.max(maxConcurrent, currentConcurrent);
      await new Promise((r) => setTimeout(r, 5));
      currentConcurrent--;
      return item * 2;
    });

    expect(maxConcurrent).toBeLessThanOrEqual(limit);
  });

  it("isolates errors per item", async () => {
    const items = [1, 2, 3, 4, 5];
    const results = await runConcurrentWithLimit(items, 2, async (x) => {
      if (x === 3) throw new Error(`Error at ${x}`);
      return x * 2;
    });

    expect(results[0]).toBe(2);
    expect(results[1]).toBe(4);
    expect(results[2]).toBeInstanceOf(Error);
    expect((results[2] as Error).message).toBe("Error at 3");
    expect(results[3]).toBe(8);
    expect(results[4]).toBe(10);
  });

  it("maintains result order", async () => {
    const items = ["a", "b", "c", "d", "e"];
    const delays = { a: 20, b: 10, c: 30, d: 5, e: 15 };
    const results = await runConcurrentWithLimit(items, 2, async (x) => {
      await new Promise((r) => setTimeout(r, delays[x as keyof typeof delays]));
      return x.toUpperCase();
    });

    expect(results).toEqual(["A", "B", "C", "D", "E"]);
  });

  it("handles empty list", async () => {
    const results = await runConcurrentWithLimit([], 5, async (x) => x);
    expect(results).toEqual([]);
  });

  it("handles concurrency limit larger than item count", async () => {
    const items = [1, 2];
    const results = await runConcurrentWithLimit(items, 100, async (x) => x * 2);
    expect(results).toEqual([2, 4]);
  });
});
