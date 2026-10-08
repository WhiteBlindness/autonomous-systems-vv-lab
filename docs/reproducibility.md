# Reprodução e compatibilidade

## Contratos preservados

O M2 mantém a versão 1 dos cenários, eventos e relatórios de invariantes. A sequência de sorteios, a ordem de entrega de mensagens e a serialização do M1 continuam a fazer parte do contrato. Os ficheiros de referência em `scenarios/sha256.json` e `scenarios/canonical-seed-42.json` permanecem fixados.

Os códigos de saída também se mantêm:

| Comando | Código | Condição |
| --- | --- | --- |
| `run` | 0 | Todos os invariantes de segurança passam |
| `run` | 2 | Pelo menos um invariante falha |
| `run` | 1 | Erro de configuração ou execução |
| `replay` | 0 | Reconstrução exata, incluindo violações existentes |
| `replay` | 1 | Registo inválido ou resultado divergente |

Uma missão estagnada com segurança PASS mantém código 0. Consultar sempre `mission.json` quando a conclusão da missão for relevante.

## Reconstruir a análise da missão

```sh
cargo run -- run scenarios/basic-mission.json --seed 42 --output runs/basic
cargo run -- replay runs/basic/events.json
cargo run -- analyze runs/basic/events.json --output runs/basic/mission-replayed.json
```

`analyze` reproduz os eventos antes de recalcular a análise. Não lê as conclusões de `mission.json`. Os dois relatórios de missão devem ter bytes iguais quando usam a mesma política de progresso. A referência `source_final_hash` identifica a sequência de eventos analisada.

A política de progresso tem parâmetros explícitos, independentes do cenário v1:

```sh
cargo run -- run scenarios/m2/d-safe-held-stall.json --seed 42 --output runs/hold --stall-window-ticks 8 --min-progress-mm 1
cargo run -- analyze runs/hold/events.json --output runs/hold/mission-replayed.json --stall-window-ticks 8 --min-progress-mm 1
```

Alterar a política pode mudar a classificação da missão; não muda os eventos nem os invariantes. Para reproduzir a classificação, conservar também a política indicada no relatório.

## Limites das garantias

A reprodução rejeita eventos corrompidos, ausentes ou fora de ordem. Reexecuta movimento, decisões e verificações, compara todo o resultado e confirma as entradas geradas pela semente.

A integração contínua compara saídas canónicas de Linux e Windows. Os tempos de execução, diretórios de campanha e orçamentos de tempo real pertencem às ferramentas externas; não entram na evolução da simulação. Um limite de tempo atingido pode interromper a pesquisa de reduções mais cedo numa máquina lenta. A ordem dos candidatos e a decisão de aceitação permanecem determinísticas para a mesma entrada, semente e política.

SHA-256 deteta alterações, mas não autentica a origem. Um registo reconstruído com novos hashes representa outro artefacto; a proveniência exige uma soma de controlo independente ou assinatura.
