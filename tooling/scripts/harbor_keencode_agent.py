"""Harbor adapter for the development-only KeenCode benchmark runner."""

import json
import os
import shlex
import tempfile
from pathlib import Path

import certifi
from harbor.agents.base import BaseAgent
from harbor.environments.base import BaseEnvironment
from harbor.models.agent.context import AgentContext


class KeenCodeAgent(BaseAgent):
    @staticmethod
    def name() -> str:
        return "keencode"

    def version(self) -> str:
        return os.environ.get("KEENCODE_BENCH_COMMIT", "development")

    @classmethod
    def preflight(cls, kwargs=None, env=None) -> None:
        source = {**os.environ, **(env or {})}
        for name in (
            "KEENCODE_BENCH_RUNNER",
            "KEENCODE_BENCH_API_KEY",
            "KEENCODE_BENCH_BASE_URL",
            "KEENCODE_BENCH_MODEL",
        ):
            if not source.get(name):
                raise ValueError(f"缺少 {name}")
        runner = Path(source["KEENCODE_BENCH_RUNNER"])
        if not runner.is_absolute() or not runner.is_file():
            raise ValueError("KEENCODE_BENCH_RUNNER 必须是现有 Linux Runner 的绝对路径")

    async def setup(self, environment: BaseEnvironment) -> None:
        runner_value = self._get_env("KEENCODE_BENCH_RUNNER")
        if not runner_value:
            raise ValueError("缺少 KEENCODE_BENCH_RUNNER")
        runner = Path(runner_value)
        await environment.exec(
            command="mkdir -p /installed-agent",
            user="root",
        )
        await environment.upload_file(runner, "/installed-agent/keencode-bench")
        await environment.upload_file(
            Path(certifi.where()), "/installed-agent/ca-certificates.crt"
        )
        result = await environment.exec(
            command="chmod 755 /installed-agent/keencode-bench && /installed-agent/keencode-bench </dev/null || test $? -eq 1",
            user="root",
        )
        if result.return_code != 0:
            raise RuntimeError(f"KeenCode Runner 无法在题目环境启动: {result.stderr}")

    async def run(
        self,
        instruction: str,
        environment: BaseEnvironment,
        context: AgentContext,
    ) -> None:
        timeout_ms = int(self._get_env("KEENCODE_BENCH_TIMEOUT_MS") or "1800000")
        api_key = self._get_env("KEENCODE_BENCH_API_KEY")
        base_url = self._get_env("KEENCODE_BENCH_BASE_URL")
        model = self._get_env("KEENCODE_BENCH_MODEL")
        if not api_key or not base_url or not model:
            raise ValueError("KeenCode benchmark 模型配置不完整")
        request = {
            "cwd": "/app",
            "storage": str(self.environment_logs_dir / "runtime"),
            "prompts": [instruction],
            "model": model,
            "baseUrl": base_url,
            "apiBackend": "messages",
            "timeoutMs": timeout_ms,
            "contextWindowTokens": int(
                self._get_env("KEENCODE_BENCH_CONTEXT_WINDOW_TOKENS") or "200000"
            ),
            "maxOutputTokens": int(
                self._get_env("KEENCODE_BENCH_MAX_OUTPUT_TOKENS") or "16384"
            ),
        }
        with tempfile.TemporaryDirectory(prefix="keencode-harbor-") as directory:
            request_path = Path(directory) / "request.json"
            request_path.write_text(json.dumps(request), encoding="utf-8")
            await environment.upload_file(request_path, "/installed-agent/request.json")
        result = await environment.exec(
            command=(
                "/installed-agent/keencode-bench "
                "< /installed-agent/request.json "
                f"> {shlex.quote(str(self.environment_logs_dir / 'runner.stdout.log'))} "
                f"2> {shlex.quote(str(self.environment_logs_dir / 'runner.stderr.log'))}"
            ),
            env={
                "KEENCODE_BENCH_API_KEY": api_key,
                "SSL_CERT_FILE": "/installed-agent/ca-certificates.crt",
            },
            timeout_sec=(timeout_ms // 1000) + 30,
        )
        if result.return_code != 0:
            error = await environment.exec(
                command=f"tail -c 16000 {shlex.quote(str(self.environment_logs_dir / 'runner.stderr.log'))}"
            )
            raise RuntimeError(
                f"KeenCode Runner 退出码 {result.return_code}: {error.stdout or error.stderr}"
            )
