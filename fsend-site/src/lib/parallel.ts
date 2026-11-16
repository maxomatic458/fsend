/** Runs `task` for each of `items`, at most `limit` at a time; fails with the first failure. */
export async function eachAtOnce<T>(
    items: T[],
    limit: number,
    task: (item: T) => Promise<void>,
): Promise<void> {
    let next = 0;
    const run = async () => {
        while (next < items.length) await task(items[next++]);
    };
    await Promise.all(Array.from({ length: Math.min(limit, items.length) }, run));
}
