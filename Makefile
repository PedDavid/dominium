# Tailwind v4 standalone CLI (no Node.js needed):
# https://github.com/tailwindlabs/tailwindcss/releases/tag/v4.3.3
TAILWIND ?= tailwindcss
HELM ?= helm
PROMTOOL ?= promtool

.PHONY: css check demo helm-lint alerts-test

css: ## Rebuild assets/dist/app.css from assets/app.css and the templates
	$(TAILWIND) -i assets/app.css -o assets/dist/app.css --minify

check: ## What CI runs for the Rust code
	cargo fmt --check
	cargo clippy --all-targets --locked -- -D warnings
	cargo test --locked

demo: ## Run the UI with sample data
	cargo run -- --demo

helm-lint: ## Lint and render the chart
	$(HELM) lint deploy/helm/dominium
	$(HELM) template dom deploy/helm/dominium --set prometheusRule.enabled=true \
	  --set serviceMonitor.enabled=true --set grafanaDashboard.enabled=true \
	  --set cloudflare.accountId=abc --set cloudflare.token.existingSecret=cf >/dev/null

alerts-test: ## Render the alert rules and run their promtool unit tests
	mkdir -p target
	$(HELM) template dom deploy/helm/dominium --set prometheusRule.enabled=true \
	  --set publicUrl=https://domains.example.com --show-only templates/prometheusrule.yaml \
	  | sed -n '/^spec:/,$$p' | tail -n +2 | sed 's/^  //' > target/alerts.yaml
	$(PROMTOOL) check rules target/alerts.yaml
	$(PROMTOOL) test rules deploy/tests/alerts_test.yaml
