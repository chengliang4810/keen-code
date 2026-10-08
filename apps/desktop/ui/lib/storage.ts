import { invoke } from "@tauri-apps/api/core";
import {
  LazyStore as PluginStore,
  type StoreOptions,
} from "@tauri-apps/plugin-store";
import type { UnlistenFn } from "@tauri-apps/api/event";

export type StoreId =
  | "settings"
  | "sessions"
  | "agents"
  | "projects"
  | "todos"
  | "themes"
  | "navigation"
  | "taskSidebars"
  | "ui";
type StoragePaths = { stores: Record<StoreId, string> };
let paths: Promise<StoragePaths> | undefined;

function storagePaths(): Promise<StoragePaths> {
  if (!paths) {
    paths = invoke<StoragePaths>("storage_paths").catch((error) => {
      paths = undefined;
      throw error;
    });
  }
  return paths;
}

export class LazyStore {
  private pending: Promise<PluginStore> | undefined;

  constructor(
    private readonly id: StoreId,
    private readonly options?: StoreOptions,
  ) {}

  private store(): Promise<PluginStore> {
    if (!this.pending) {
      this.pending = storagePaths()
        .then(async ({ stores }) => {
          const path = stores[this.id];
          if (!path)
            throw new Error(`RCode storage path is missing: ${this.id}`);
          const store = new PluginStore(path, {
            ...this.options,
            createNew: true,
          });
          await store.init();
          try {
            await store.reload({ ignoreDefaults: true });
          } catch (error) {
            try {
              if (
                await invoke<boolean>("storage_store_missing", { id: this.id })
              )
                return store;
            } catch (validationError) {
              await store.close();
              throw validationError;
            }
            await store.close();
            throw error;
          }
          return store;
        })
        .catch((error) => {
          this.pending = undefined;
          throw error;
        });
    }
    return this.pending;
  }

  async get<T>(key: string): Promise<T | undefined> {
    return (await this.store()).get<T>(key);
  }

  async reload(): Promise<void> {
    const store = await this.store();
    try {
      await store.reload({ ignoreDefaults: true });
    } catch (error) {
      try {
        if (
          (await invoke<boolean>("storage_store_missing", { id: this.id })) &&
          (await store.entries()).length === 0
        )
          return;
      } catch (validationError) {
        await store.close();
        this.pending = undefined;
        throw validationError;
      }
      await store.close();
      this.pending = undefined;
      throw error;
    }
  }
  async set(key: string, value: unknown): Promise<void> {
    await (await this.store()).set(key, value);
  }
  async delete(key: string): Promise<boolean> {
    return (await this.store()).delete(key);
  }
  async entries<T>(): Promise<[string, T][]> {
    return (await this.store()).entries<T>();
  }
  async save(): Promise<void> {
    await (await this.store()).save();
  }
  async onChange<T>(
    cb: (key: string, value: T | undefined) => void,
  ): Promise<UnlistenFn> {
    return (await this.store()).onChange<T>(cb);
  }
}
