import { afterEach, describe, expect, it, vi } from "vitest";
import * as api from "@/lib/api";

describe("供应商 API Key 本地持久化契约", () => {
  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it("保存时原样提交非空 Key，禁止前端静默裁剪", async () => {
    const invoke = vi.fn().mockResolvedValue({});
    vi.stubGlobal("window", {
      __TAURI_INTERNALS__: { invoke },
    });

    await api.providersUpsert({
      id: "provider",
      models: ["model"],
      baseUrl: "https://api.example.com/v1",
      name: "Provider",
      apiKey: " padded-key ",
      apiBackend: "responses",
      createOnly: true,
    });

    expect(invoke).toHaveBeenCalledWith(
      "providers_upsert",
      {
        id: "provider",
        models: ["model"],
        baseUrl: "https://api.example.com/v1",
        name: "Provider",
        apiKey: " padded-key ",
        apiBackend: "responses",
        contextWindows: {},
        context1m: {},
        createOnly: true,
      },
      undefined,
    );
  });

  it("未提供 Key 时提交 null，由后端清空该供应商密钥或为新供应商使用无认证", async () => {
    const invoke = vi.fn().mockResolvedValue({});
    vi.stubGlobal("window", {
      __TAURI_INTERNALS__: { invoke },
    });

    await api.providersUpsert({
      id: "provider",
      models: ["model"],
      baseUrl: "https://api.example.com/v1",
      apiBackend: "responses",
      createOnly: false,
    });

    expect(invoke).toHaveBeenCalledWith(
      "providers_upsert",
      expect.objectContaining({ apiKey: null }),
      undefined,
    );
  });

  it("编辑供应商拉取模型时传递 providerId，由后端读取唯一密钥源", async () => {
    const invoke = vi.fn().mockResolvedValue({ models: [] });
    vi.stubGlobal("window", {
      __TAURI_INTERNALS__: { invoke },
    });

    await api.providersListModels({
      baseUrl: "https://api.example.com/v1",
      providerId: "provider",
      apiBackend: "responses",
    });

    expect(invoke).toHaveBeenCalledWith(
      "providers_list_models",
      {
        baseUrl: "https://api.example.com/v1",
        apiKey: null,
        providerId: "provider",
        apiBackend: "responses",
      },
      undefined,
    );
  });
});
