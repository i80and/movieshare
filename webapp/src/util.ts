export function pollUntil(
  conditionFn: () => Promise<boolean> | boolean,
  interval: number = 250,
): Promise<void> {
  return new Promise((resolve, reject) => {
    const id = setInterval(async () => {
      try {
        if (await conditionFn()) {
          clearInterval(id);
          resolve();
        }
      } catch (err) {
        clearInterval(id);
        reject(err);
      }
    }, interval);
  });
}

export class TypedEventTarget<Events extends Record<string, unknown>>
  extends EventTarget {
  // Type-safe addEventListener wrapper
  on<K extends keyof Events>(
    type: K,
    listener: (event: CustomEvent<Events[K]>) => void,
  ) {
    super.addEventListener(type as string, listener as EventListener);
  }

  // Type-safe dispatchEvent wrapper
  emit<K extends keyof Events>(type: K, detail: Events[K]) {
    return super.dispatchEvent(new CustomEvent(type as string, { detail }));
  }
}
