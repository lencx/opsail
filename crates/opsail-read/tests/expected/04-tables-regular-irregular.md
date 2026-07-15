# Seasonal Reading Tables

Tables preserve relationships that disappear when cells are flattened into a single stream. The first example has a conventional header and complete rows. The second deliberately uses spanning cells and a missing value so the conversion path must remain stable even when the source is irregular.

**Regular monthly readings**

| Month | Rainfall | Open days |
| ----- | -------- | --------- |
| April | 82 mm | 22 |
| May | 61 mm | 27 |
| June | 94 mm | 24 |

## Irregular maintenance record

**Work windows and shared notes**

**Window · Measurements · Note**

**Before · After**

Morning · 14.2 · 14.6 · Sensor cleaned and checked

Afternoon · 14.6

Evening reading postponed · Path closed

A readable result should preserve the regular grid as a Markdown table and handle the irregular grid without panicking, duplicating distant prose, or silently discarding every value. Captions should stay near the table they describe, and line breaks inside a cell should remain understandable.
