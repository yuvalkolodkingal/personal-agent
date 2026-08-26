# Performance evidence

`python scripts/verify-performance.py` runs 100 networking-disabled replay samples and checks the required p50, p95, maximum, and sample-count fields. The committed machine report is `performance-report.json`; CI reruns the probe rather than trusting the committed values.

The replay gate covers hotkey state transition, enrolled wake detection, internal playback cancellation, and the full local recognizer-to-synthesizer orchestration path. Its limits are the product budgets: 100 ms, 250 ms, 50 ms, and 500 ms respectively.

Physical end-to-end barge-in, cloud first-audio latency, idle CPU, idle resident memory, and warm graphical startup remain hardware measurements. A release operator must run them on each reference device and store the same distribution schema. Replay numbers must never be presented as physical microphone, speaker, network, or UI-startup measurements.

Current deterministic evidence: [performance-report.json](performance-report.json).
