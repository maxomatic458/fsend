/** Runs `fn` once `signal` aborts, right away if it already has. */
export function onAbort(signal: AbortSignal, fn: () => void): void {
  if (signal.aborted) fn();
  else signal.addEventListener("abort", fn, { once: true });
}

/** Settles like `promise`, or rejects once `signal` aborts. */
export function abortable<T>(
  promise: Promise<T>,
  signal: AbortSignal,
): Promise<T> {
  return new Promise((resolve, reject) => {
    const abort = () => reject(new Error("Aborted"));
    onAbort(signal, abort);
    promise
      .then(resolve, reject)
      .finally(() => signal.removeEventListener("abort", abort));
  });
}
