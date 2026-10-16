# Seguimiento de robustez

Alcance: hallazgos F1–F15 de [la auditoría](BEP-AUDIT-2026-10-16.md).
Cada hallazgo sigue implementación → self-review → correcciones → pruebas → merge.
Las referencias de líneas de la auditoría describen el commit base, no el código posterior.

| Hallazgo | Estado | Evidencia |
| --- | --- | --- |
| F1 — aislamiento de archivos | Implementado y revisado; 35 pruebas pasan, 1 de red pública ignorada | `tests/storage_confinement.rs` |
| F2 — asociación de respuestas UDP | Pendiente | |
| F3 — límites de entradas | Pendiente | |
| F4 — integridad y durabilidad de reanudación | Pendiente | |
| F5 — capacidad, cancelación y equidad de subida | Pendiente | |
| F6 — niveles y failover de trackers | Pendiente | |
| F7 — recuperación y URLData UDP | Pendiente | |
| F8 — extensiones y PEX | Pendiente | |
| F9 — ciclo de vida y persistencia | Pendiente | |
| F10 — contabilidad de escrituras parciales | Pendiente | |
| F11 — recuperación de metadata persistida | Pendiente | |
| F12 — pausa y borrado durante inicialización | Pendiente | |
| F13 — límites al servir metadata | Pendiente | |
| F14 — secuencia al recibir metadata | Pendiente | |
| F15 — buffers para mapas de piezas | Pendiente | |

## F1: confinamiento de almacenamiento

Las operaciones sobre rutas de un torrent usan un descriptor de directorio compartido y `cap-std`, incluyendo creación, reapertura, restauración y borrado. Las lecturas y escrituras posteriores usan el descriptor del archivo abierto. La carpeta raíz elegida por el usuario es la frontera de confianza; los enlaces relativos que permanecen dentro de ella son admisibles. Esto no intenta aislar hardlinks ni un actor que pueda modificar directamente cualquier archivo del usuario.

Self-review: la primera implementación tenía un bypass de `O_NOFOLLOW` cuando la raíz terminaba en `/`, generado por `join("")`. La regresión falló; normalizar componentes antes de abrir la raíz corrigió el defecto. Se conserva la carpeta raíz vacía al borrar, porque resolver de nuevo su nombre podría borrar una carpeta distinta tras un rename. Los descendientes del torrent sí se eliminan. La raíz se fija al abrirla; la prueba de sustitución verifica que el borrado sigue el directorio original.

Pruebas: enlaces absolutos y relativos de archivo/directorio, creación y reutilización, raíz simbólica, restauración, sustitución de raíz/descendiente antes de borrar, y 500 aperturas/escrituras/borrados durante intercambios concurrentes. Los archivos externos conservan contenido, tamaño y existencia. Todas las pruebas utilizan directorios temporales sin trackers externos.

Validación final: `cargo test --manifest-path src-tauri/Cargo.toml --locked` — 29 pruebas de aplicación y 6 regresiones de almacenamiento pasan; una prueba de red pública ignorada. `cargo check` y `git diff --check` pasan. Sin hallazgos bloqueantes restantes en el diff F1.
