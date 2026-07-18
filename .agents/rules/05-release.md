# 05 — 发布流程（四渠道）

1. **GitHub Release**：打 tag `v*` → `release.yml` 自动构建 musl 静态二进制
   （装 musl-tools → build → strip → tar.gz + sha256）→ 创建 Release →
   大版本浮动 tag（如 `v0`）force-move 到最新。
2. **crates.io 主包**：`cargo publish -p hoverstare`。
3. **crates.io 别名包**：主包在 registry 可见后，`cargo publish -p bugbot`，
   版本号跟随主包（别名 crate 依赖主包同版本）。
4. **Marketplace**：无 API，Release 编辑页手动勾选
   "Publish this Action to the GitHub Marketplace"；
   元数据 = 根目录 `action.yml`（name 必须唯一，不能撞 GitHub 用户/组织名——
   改名前先双查：`github.com/<name>` 与 `github.com/marketplace/actions/<name>`）。
5. 发版前：`cargo publish --dry-run --allow-dirty` 验证两个包都能打包编译。
6. tag 打错：删远端 tag + 对应 release → 修正 → 重打（force-push tag）。
7. 品牌身份模式：HoverStare GitHub App（无 webhook、Public）+ action 的
   app_id/app_private_key 输入 → create-github-app-token 换 installation token。
   注意该 action 会替换后续步骤的 github.token 上下文，cache 步骤必须显式固定
   Actions token；App token 可完整 resolve review threads（无 GITHUB_TOKEN 限制）。
