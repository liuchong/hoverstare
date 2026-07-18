<p align="center">
  <img src=".github/assets/logo.svg" width="128" alt="bugbot logo" />
  <h1 align="center">Bugbot</h1>
  <p align="center">
    <b>ИИ-ревью кода, которое действительно читает ваш репозиторий.</b>
  </p>
  <p align="center">
    <a href="https://github.com/liuchong/bugbot/actions/workflows/ci.yml"><img src="https://github.com/liuchong/bugbot/actions/workflows/ci.yml/badge.svg" alt="CI" /></a>
    <a href="https://github.com/liuchong/bugbot/releases"><img src="https://img.shields.io/github/v/release/liuchong/bugbot" alt="release" /></a>
    <a href="https://crates.io/crates/bugbot"><img src="https://img.shields.io/crates/v/bugbot" alt="crates.io" /></a>
    <a href="https://license.pub/1pl/"><img src="https://img.shields.io/badge/license-1PL-green" alt="license 1PL" /></a>
  </p>
  <p align="center">
    <a href="README.md">English</a> ·
    <a href="README.zh-CN.md">简体中文</a> ·
    <b>Русский</b> ·
    <a href="README.fr.md">Français</a> ·
    <a href="README.de.md">Deutsch</a> ·
    <a href="README.es.md">Español</a>
  </p>
</p>

<br/>

Bugbot — это ИИ-бот для код-ревью пул-реквестов GitHub, написанный на Rust и
поставляемый как единый статический бинарник, работающий как GitHub Action.
Вместо того чтобы отправить diff модели за один проход, ревьюер **читает ваш
репозиторий, как это сделал бы человек** — открывает файлы с контекстом, ищет
места вызова через grep, сравнивает с базовой веткой — и только потом делает
выводы. Многопроходное голосование и независимый верификатор снижают число
ложных срабатываний, а каждая найденная проблема отслеживается между коммитами
до её исправления.

## Почему Bugbot?

- 🔍 **Понимает репозиторий, а не только diff.** У модели есть набор
  read-only инструментов (`read_file` / `grep` / `glob` / `show_base_file`),
  и она проверяет подозрения перед отчётом. Находит баги *за пределами* diff —
  например, когда изменённая функция ломает вызывающий её код в других файлах.
- 🗳️ **Многопроходное голосование + верификатор.** Три независимых прохода
  (корректность / конкурентность / безопасность) голосуют по находкам;
  находки с одним голосом проходят независимую проверку с доступом к инструментам.
- 📌 **Точные встроенные комментарии.** Номера строк проверяются по реальному
  diff и привязываются к ближайшей допустимой строке — комментарии попадают
  именно туда, где находится баг.
- 🔁 **Инкрементальные ревью.** После пуша исправления Bugbot проверяет только
  дельту, отмечает исправленные находки как resolved (или оставляет
  «✅ исправление подтверждено») и никогда не повторяется.
- 🛡️ **Fail-open по дизайну.** Сетевые сбои, рейт-лимиты или нестабильная
  модель никогда не заблокируют ваш CI.
- 🔑 **BYOK.** Свой ключ: Anthropic или любой OpenAI-совместимый endpoint
  (Kimi, DeepSeek, OpenRouter, …). Код идёт напрямую к вашему провайдеру.

## Как это работает

```mermaid
flowchart LR
    A[PR opened / synchronized] --> B{skip?}
    B -->|draft / bot / empty diff| Z((exit 0))
    B --> C[fetch diff]
    C --> D{prior review?}
    D -->|yes| E[delta diff]
    D -->|no| F[full diff]
    E --> G
    F --> G["N parallel review passes<br/>(read-only repo tools)"]
    G --> H[cluster & vote]
    H --> I[verifier pass]
    I --> J[validate & anchor lines]
    J --> K["post review<br/>+ resolve fixed threads<br/>+ status checks"]
```

Каждый встроенный комментарий содержит скрытый отпечаток (хэш
`путь + строка кода + заголовок`). При следующем пуше Bugbot сравнивает со
своим прошлым ревью, спрашивает у модели, какие открытые находки исправлены,
и обрабатывает эти треды — дрейф номеров строк не влияет на отпечаток.

## Быстрый старт (2 минуты)

**1. Добавьте workflow** — `.github/workflows/bugbot.yml`:

```yaml
name: Bugbot
on:
  pull_request:
    types: [opened, reopened, synchronize]
  issue_comment:
    types: [created]
  pull_request_review_comment:
    types: [created]

permissions:
  contents: read
  pull-requests: write
  statuses: write

concurrency:
  group: bugbot-${{ github.event.pull_request.number || github.event.issue.number }}
  cancel-in-progress: true

jobs:
  bugbot:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0
      - uses: liuchong/bugbot@v0
        env:
          GITHUB_TOKEN: ${{ secrets.GITHUB_TOKEN }}
          OPENAI_API_KEY: ${{ secrets.BUGBOT_LLM_KEY }}
          OPENAI_BASE_URL: ${{ vars.BUGBOT_LLM_BASE_URL }}
          BUGBOT_MODEL: ${{ vars.BUGBOT_MODEL }}   # например kimi-for-coding
```

**2. Настройте доступ к LLM** (на выбор):

| Провайдер | Настройки |
|---|---|
| **Anthropic** | секрет `ANTHROPIC_API_KEY` (модель по умолчанию `claude-sonnet-4-6`) |
| **OpenAI-совместимый** (Kimi, DeepSeek, OpenRouter…) | секрет `OPENAI_API_KEY`, переменная `OPENAI_BASE_URL` (например `https://api.kimi.com/coding/v1`), имя модели через `BUGBOT_MODEL` или `model` в `.github/bugbot.toml` |

> ⚠️ Для OpenAI-совместимого endpoint **обязательно** укажите имя модели —
> модели по умолчанию `claude-sonnet-4-6` там не существует.

**3. (Опционально) Конфиг репозитория** — `.github/bugbot.toml`, все поля необязательны:

```toml
model = "kimi-for-coding"             # основная модель ревью
reformat_model = "kimi-for-coding-highspeed"  # дешёвая модель для восстановления вывода
passes = 3                            # параллельные проходы; 1 отключает голосование
verify = true                         # верификатор для находок с одним голосом
severity_threshold = "medium"         # ниже порога — только раздел Nitpicks
ignore = ["*.lock", "**/dist/**", "**/*.min.js"]
max_diff_kb = 400                     # бюджет diff (усечение по приоритету)
max_tool_calls = 20                   # бюджет вызовов инструментов
timeout_secs = 900
review_drafts = false
fail_closed = false                   # true — ошибки анализа ломают CI
status_checks = false                 # писать проверки bugbot / bugbot-findings
set_temperature = true                # false для endpoint'ов, принимающих только температуру по умолчанию
instructions = ""                     # фокус ревью команды, добавляется в системный промпт
```

## Команды `@bugbot`

В комментариях к PR (только для коллабораторов репозитория):

| Команда | Действие |
|---|---|
| `@bugbot review` | Принудительный полный повторный ревью |
| `@bugbot explain` | Ответить в треде простым объяснением находки |
| `@bugbot help` | Список команд |

## Частые вопросы

**Ошибки прав при публикации ревью?**
Проверьте `permissions` в workflow (нужен `pull-requests: write`) и что в
*Settings → Actions → General → Workflow permissions* выбрано "Read and write".

**"model not found"?**
Вы настроили OpenAI-совместимый endpoint, но не указали имя модели.
Задайте `BUGBOT_MODEL` (или `model` в `bugbot.toml`).

**400 / invalid temperature?**
Endpoint принимает только температуру по умолчанию.
Установите `set_temperature = false` в `bugbot.toml`.

**Исправленные находки не закрываются?**
Ограничение платформы GitHub: стандартный `GITHUB_TOKEN` не может вызывать
`resolveReviewThread`. Bugbot отвечает в треде «✅ исправление подтверждено».
Для полного resolve сохраните classic PAT (`repo` scope) как секрет `GH_PAT`
и передайте его в env workflow.

**GitHub Enterprise?**
Установите `GITHUB_API_URL=https://<ваш-ghe-хост>/api/v3`.

## Локальная разработка

```bash
# Полный dry-run ревью публичного PR (без публикации)
export OPENAI_API_KEY=... OPENAI_BASE_URL=... BUGBOT_MODEL=...
cargo run -- review --repo owner/repo --pr 123 --dry-run

# Ревью локального diff-файла (печатает трассу вызовов инструментов)
cargo run --example local_review -- path/to.diff [base_ref]

cargo test                                   # модульные + контрактные тесты httpmock
cargo clippy --all-targets -- -D warnings
cargo fmt
```

Спецификации и план вех — в [`specs/`](specs/README.md), единственный
источник правды по проектным решениям.

## Лицензия

[1PL — One Public License](https://license.pub/1pl/)
