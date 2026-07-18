<p align="center">
  <img src=".github/assets/logo.svg" width="128" alt="bugbot logo" />
  <h1 align="center">Bugbot</h1>
  <p align="center">
    <b>Revisión de código con IA que realmente lee tu repositorio.</b>
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
    <a href="README.ru.md">Русский</a> ·
    <a href="README.fr.md">Français</a> ·
    <a href="README.de.md">Deutsch</a> ·
    <b>Español</b>
  </p>
</p>

<br/>

Bugbot es un bot de revisión de código con IA para pull requests de GitHub,
escrito en Rust y distribuido como un único binario estático que se ejecuta
como GitHub Action. En lugar de lanzar el diff a un modelo de una sola vez, su
revisor **lee tu repositorio como lo haría un humano** — abre archivos de
contexto, busca sitios de llamada con grep, compara con la rama base — antes
de concluir. Una votación multipaso más un verificador independiente mantiene
bajos los falsos positivos, y cada hallazgo se rastrea entre commits hasta que
se corrige.

## ¿Por qué Bugbot?

- 🔍 **Consciente del repo, no solo del diff.** El modelo dispone de
  herramientas de solo lectura (`read_file` / `grep` / `glob` /
  `show_base_file`) y verifica sus sospechas antes de reportar. Detecta bugs
  que se esconden *fuera* del diff — como una función modificada cuyos
  invocadores se rompen dos archivos más allá.
- 🗳️ **Votación multipaso + verificador.** Tres pasadas independientes
  (corrección / concurrencia / seguridad) votan los hallazgos; los de un solo
  voto deben superar un verificador independiente con acceso a herramientas.
- 📌 **Comentarios en línea precisos.** Los números de línea se validan contra
  el diff real y se ajustan al ancla válida más cercana — los comentarios caen
  exactamente donde está el bug.
- 🔁 **Revisiones incrementales.** Al empujar una corrección, Bugbot revisa
  solo el delta, marca los hallazgos corregidos como resueltos (o deja una
  nota «✅ corrección confirmada») y nunca se repite.
- 🛡️ **Fail-open por diseño.** Problemas de red, límites de tasa o un modelo
  inestable nunca bloquearán tu CI.
- 🔑 **BYOK.** Trae tu propia clave: Anthropic o cualquier endpoint compatible
  con OpenAI (Kimi, DeepSeek, OpenRouter, …). El código va directo a tu
  proveedor.

## Cómo funciona

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

Cada comentario en línea lleva una huella oculta (hash de
`ruta + línea de código + título`). En el siguiente push, Bugbot compara con su
revisión anterior, pregunta al modelo qué hallazgos abiertos están corregidos
y procesa esos hilos — inmune a la deriva de números de línea.

## Inicio rápido (2 minutos)

**1. Añade el workflow** — `.github/workflows/bugbot.yml`:

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
          BUGBOT_MODEL: ${{ vars.BUGBOT_MODEL }}   # p. ej. kimi-for-coding
```

**2. Configura las credenciales LLM** (elige una):

| Proveedor | Configuración |
|---|---|
| **Anthropic** | secreto `ANTHROPIC_API_KEY` (modelo por defecto `claude-sonnet-4-6`) |
| **Compatible con OpenAI** (Kimi, DeepSeek, OpenRouter…) | secreto `OPENAI_API_KEY`, variable `OPENAI_BASE_URL` (p. ej. `https://api.kimi.com/coding/v1`), nombre del modelo vía `BUGBOT_MODEL` o `model` en `.github/bugbot.toml` |

> ⚠️ Con un endpoint compatible con OpenAI **debes** definir el nombre del
> modelo — el predeterminado `claude-sonnet-4-6` no existe ahí.

**3. (Opcional) Config del repo** — `.github/bugbot.toml`, todos los campos opcionales:

```toml
model = "kimi-for-coding"             # modelo principal de revisión
reformat_model = "kimi-for-coding-highspeed"  # modelo barato para reparar la salida
passes = 3                            # pasadas en paralelo; 1 desactiva la votación
verify = true                         # verificador para hallazgos de un solo voto
severity_threshold = "medium"         # por debajo → solo sección Nitpicks
ignore = ["*.lock", "**/dist/**", "**/*.min.js"]
max_diff_kb = 400                     # presupuesto de diff (truncado por prioridad)
max_tool_calls = 20                   # presupuesto de llamadas a herramientas
timeout_secs = 900
review_drafts = false
fail_closed = false                   # true → los fallos de análisis rompen la CI
status_checks = false                 # escribir checks bugbot / bugbot-findings
set_temperature = true                # false para endpoints que solo aceptan la temperatura por defecto
instructions = ""                     # enfoque de revisión del equipo, inyectado en el prompt de sistema
```

## Comandos `@bugbot`

Publica en un PR (solo colaboradores del repo):

| Comando | Qué hace |
|---|---|
| `@bugbot review` | Fuerza una revisión completa |
| `@bugbot explain` | Responde en el hilo con una explicación sencilla del hallazgo |
| `@bugbot help` | Lista de comandos |

## Preguntas frecuentes

**¿Errores de permisos al publicar?**
Revisa los `permissions` del workflow (`pull-requests: write` requerido) y que
*Settings → Actions → General → Workflow permissions* esté en "Read and write".

**¿"model not found"?**
Configuraste un endpoint compatible con OpenAI pero no el nombre del modelo.
Define `BUGBOT_MODEL` (o `model` en `bugbot.toml`).

**¿400 / invalid temperature?**
Tu endpoint solo acepta la temperatura por defecto. Pon
`set_temperature = false` en `bugbot.toml`.

**¿Los hallazgos corregidos no se resuelven?**
Una limitación de la plataforma GitHub: el `GITHUB_TOKEN` por defecto no puede
llamar a `resolveReviewThread`. Bugbot responde entonces «✅ corrección
confirmada» en el hilo. Para resolución completa, guarda un PAT clásico
(`repo` scope) como secreto `GH_PAT` y pásalo en el env del workflow.

**¿GitHub Enterprise?**
Define `GITHUB_API_URL=https://<tu-host-ghe>/api/v3`.

## Desarrollo local

```bash
# Dry-run de una revisión completa de un PR público (sin publicar)
export OPENAI_API_KEY=... OPENAI_BASE_URL=... BUGBOT_MODEL=...
cargo run -- review --repo owner/repo --pr 123 --dry-run

# Revisar un archivo diff local (imprime la traza de llamadas a herramientas)
cargo run --example local_review -- path/to.diff [base_ref]

cargo test                                   # tests unitarios + de contrato httpmock
cargo clippy --all-targets -- -D warnings
cargo fmt
```

Las specs y el plan de hitos están en [`specs/`](specs/README.md) — la fuente
única de verdad para las decisiones de diseño.

## Licencia

[1PL — One Public License](https://license.pub/1pl/)
