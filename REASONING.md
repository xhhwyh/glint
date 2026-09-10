# Model reasoning effort

Verified against provider documentation and public model metadata on 2026-09-09.
Glint exposes distinct effective effort levels, rather than adding aliases that
map to the same model behavior. The capability is keyed by provider and exact
model ID; an OpenAI-compatible URL alone does not establish support.

| Provider | Shipped models | Selectable levels | Default |
| --- | --- | --- | --- |
| OpenAI (ChatGPT subscription) | Configured account models | Account model catalog | Account model catalog |
| DeepSeek | deepseek-v4-flash, deepseek-v4-pro | low, high, max | high |
| Volcengine | doubao-seed-2-1-pro-260628, doubao-seed-2-1-turbo-260628 | minimal, low, medium, high | high |
| Volcengine | doubao-seed-2-0-pro-260215, doubao-seed-2-0-lite-260428, doubao-seed-2-0-mini-260428, doubao-seed-2-0-code-preview-260215 | minimal, low, medium, high | medium |
| Zhipu | glm-5.2 | high, max | max |
| OpenRouter | deepseek/deepseek-v4-flash (0423 deployment) | high, xhigh | high |

`minimal` on Volcengine disables thinking. GLM-5.2 accepts additional compatibility
values: low/medium map to high and xhigh maps to max. Its none/minimal values disable
thinking rather than offering an additional reasoning depth. The selector lists
the two actual thinking strengths.

The other shipped models do not expose verified discrete effort selection:

- GLM-5.1, GLM-5-Turbo and GLM-5 offer a thinking switch.
- Kimi K2.7 Code / highspeed have always-on thinking; K2.6 and K2.5 offer thinking
  modes, but do not expose a reasoning-effort parameter.
- Qwen3.7 Max/Plus and Qwen3.6 Flash offer `enable_thinking` and `thinking_budget`.
  A token budget is not a documented low/medium/high scale, so Glint does not invent one.
- DashScope MiniMax M3 exposes adaptive/disabled thinking; M2.7 is thinking-only.
- DashScope MiMo V2.5 Pro explicitly does not support `reasoning_effort` or `thinking_budget`.
- Custom providers retain their existing behavior. Model names do not establish
  which parameters a custom gateway accepts.

## UI and persistence

In `/model`, Up/Down highlights a model. A supported model shows the same colored,
connected effort axis as OpenAI. Right focuses the axis, Left/Right adjusts it,
Backspace returns to model navigation, and Enter saves the model and effort.
Before adjustment, an unset preference follows the provider default.

API-provider preferences are remembered separately from the existing ChatGPT
subscription preferences:

```yaml
reasoning_efforts:
  deepseek:
    deepseek-v4-pro: max
  volcengine:
    doubao-seed-2-1-pro-260628: low
```

Empty maps are omitted. Unsupported stale saved values fall back to the model
default. Explicit invalid choices fail before persistence or an HTTP request.
Deleting a provider also deletes its saved efforts. Configuration IDs and secrets
are unchanged.

DeepSeek, Volcengine and Zhipu requests send `reasoning_effort` with the appropriate
`thinking.type`; OpenRouter sends `reasoning.effort`. Default selections omit these
parameters. Provider-only reasoning, encrypted content and OpenRouter reasoning details are
carried alongside assistant model history, including final answers. Optional
origin-tagged metadata is persisted in Glint's local session records so a new
provider instance or resumed session can replay it. It is not displayed as answer
text. Only the matching provider/model receives its metadata; switching providers
never forwards another provider's encrypted data. Legacy or synthetic DeepSeek
assistant messages have an empty reasoning field because no original trace was
recorded.

## Sources

- [DeepSeek thinking mode](https://api-docs.deepseek.com/guides/thinking_mode/)
- [Volcengine thinking mode and per-model mapping](https://www.volcengine.com/docs/82379/1449737?lang=zh-CN)
- [Zhipu thinking capabilities](https://docs.bigmodel.cn/cn/guide/capabilities/thinking)
- [Kimi model parameter reference](https://platform.kimi.ai/docs/api/models-overview)
- [Kimi K2.5](https://platform.kimi.ai/docs/guide/kimi-k2-5-quickstart)
- [DashScope deep thinking](https://help.aliyun.com/en/model-studio/deep-thinking)
- [DashScope MiniMax](https://help.aliyun.com/en/model-studio/minimax-api-by-minimax)
- [DashScope MiMo](https://help.aliyun.com/en/model-studio/mimo)
- [OpenRouter reasoning protocol and model metadata](https://openrouter.ai/docs/guides/best-practices/reasoning-tokens)
- [OpenRouter public model catalog](https://openrouter.ai/api/v1/models): the selected model currently advertises `supported_efforts: ["xhigh", "high"]`, `default_effort: "high"`.

Capabilities are shipped locally, so model navigation does not perform network
requests. Update the verified table and `src/reasoning.rs` together when upstream
model support changes.
