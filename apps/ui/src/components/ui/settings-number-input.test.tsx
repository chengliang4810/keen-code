import { describe, expect, it, vi } from "vitest";
import { SettingsNumberInput } from "./settings-number-input";

describe("SettingsNumberInput", () => {
  it("有效整数失焦提交，空值和越界值恢复当前设置", () => {
    const onCommit = vi.fn();
    const field = SettingsNumberInput({ value: 10, min: 1, max: 20, onCommit });
    const onBlur = field.props.onBlur!;

    for (const raw of ["", "21", "1.5", "-1"]) {
      const target = { value: raw };
      onBlur({ currentTarget: target } as never);
      expect(target.value).toBe("10");
    }
    expect(onCommit).not.toHaveBeenCalled();
    onBlur({ currentTarget: { value: "12" } } as never);
    expect(onCommit).toHaveBeenCalledOnce();
    expect(onCommit).toHaveBeenCalledWith(12);
  });
});
