# Tareas de seguridad

Auditoría base: 2026-08-08. El rate limit ya está implementado; los siguientes puntos siguen pendientes.

## Trabajo urgente de despliegue

- [ ] Rotar la contraseña de MongoDB, `ADMIN_PASSWORD` y `AUDIT_LOG_ENCRYPTION_KEY` que fueron expuestas anteriormente. La rotación de la clave de auditoría debe planificarse porque los registros antiguos no podrán descifrarse con una clave nueva.
- [ ] Reemplazar la cuenta `root` de MongoDB por un usuario de privilegios mínimos limitado a `gym_tracker`.
- [ ] Colocar MongoDB detrás de una red privada o una allowlist de firewall y exigir TLS en `MONGODB_URI`.
- [ ] Impedir el acceso directo al origen de Coolify antes de depender de `TRUST_PROXY_HEADERS=true`.

## Confidencialidad y disponibilidad del registro de auditoría

- [ ] Censurar contraseñas, `Cookie`, `Authorization` y `Set-Cookie` antes de guardar registros de auditoría.
- [ ] Añadir `Cache-Control: no-store` a las respuestas de auditoría descifradas.
- [ ] Imponer un límite pequeño al cuerpo de la petición antes de que el middleware de auditoría lo almacene o clone.
- [ ] Guardar solamente una vista previa limitada del cuerpo, su longitud y su digest, en vez de copias Base64/UTF-8 sin límite.
- [ ] Sustituir las escrituras de auditoría ilimitadas y desacopladas por una cola acotada con métricas observables de fallos.

## Autenticación y sesiones

- [ ] Evitar diferencias temporales en el login verificando un hash Argon2 ficticio cuando el usuario no exista.
- [ ] Aumentar el mínimo de las contraseñas de usuarios y administradores y rechazar contraseñas débiles o conocidas como comprometidas.
- [ ] Añadir MFA o autenticación reforzada antes de mostrar registros de auditoría descifrados.
- [ ] Reducir la duración actual de 365 días de las sesiones, limitar las sesiones simultáneas y ofrecer una operación para revocarlas todas.
- [ ] Guardar hashes de los tokens de sesión en MongoDB en vez de tokens reutilizables en texto recuperable.

## Transporte, criptografía y retención

- [ ] Establecer TLS mínimo 1.2 o 1.3 en Cloudflare y habilitar HSTS.
- [ ] Derivar claves independientes para AES e índices ciegos mediante HKDF y añadir versiones explícitas de claves para permitir rotaciones.
- [ ] Añadir limpieza TTL para `sync_mutations` y borrar esos registros cuando se elimine un usuario.
- [ ] Tratar `x-gym-client` y las IP reenviadas como metadatos forenses no confiables, salvo que hayan sido verificados por un proxy confiable.

## Garantías y cadena de suministro

- [ ] Migrar el rate limit a un almacén compartido como Redis si la API se escala a varias réplicas; el contador actual es local a cada instancia y se reinicia con el proceso.
- [ ] Añadir pruebas de integración para el bloqueo de login, CSRF, autorización administrativa, aislamiento entre usuarios, revocación de sesiones y acceso a auditorías.
- [ ] Añadir `cargo audit` y Clippy estricto al CI.
- [ ] Escanear la imagen final del contenedor, fijar los digests de las imágenes base y generar un SBOM.

## Completado

- [x] Añadir límites por cliente, por intento de login y por instancia, con respuestas `429` y `Retry-After`; las peticiones bloqueadas también quedan registradas por la auditoría existente.
