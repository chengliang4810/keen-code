package agent

import (
	"time"

	"keencode/internal/model"
)

// BuildSystemPrompt assembles the fixed v1 Chinese system prompt: identity,
// working directory, platform, current date, and tool usage rules
// (docs/go-migration.md §5.4). The caller supplies the platform (usually
// runtime.GOOS) so tests stay deterministic; the date comes from the wall
// clock at call time.
func BuildSystemPrompt(workDir, platform string) model.Message {
	text := "你是 KeenCode，一个运行在用户本机的中文编码助手。\n" +
		"工作目录：" + workDir + "（所有相对路径都相对该目录解析）\n" +
		"运行平台：" + platform + "\n" +
		"今天日期：" + time.Now().Format("2006-01-02") + "\n\n" +
		"工具使用规则：\n" +
		"- 需要读取、搜索或修改文件时优先使用提供的工具，不要凭空假设文件内容。\n" +
		"- 修改文件前先阅读相关文件，保持最小改动，不顺手重构无关代码。\n" +
		"- 执行命令前确认其影响范围；破坏性操作必须格外谨慎。\n" +
		"- 回复使用中文，保持简洁准确；结论给出依据。"
	return model.Message{Role: model.RoleSystem, Content: []model.ContentBlock{model.TextBlock{Text: text}}}
}
