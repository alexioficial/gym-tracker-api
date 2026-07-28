# Gym Tracker API

API Rust para `gym-tracker`, implementada con Actix Web y MongoDB.

## Estructura

- `src/app.rs`: estado compartido, health check y composición de rutas.
- `src/routes/`: handlers organizados por dominio (`auth`, `exercises`, `routines`, `sessions`, `admin`).
- `src/routes/shared.rs`: autenticación de petición, rol admin y protección CSRF reutilizables.
- `src/validation.rs`: límites y validadores comunes.
- `src/auth.rs`, `src/db.rs`, `src/models.rs`: criptografía/sesiones, persistencia y contratos MongoDB/API.

## Seguridad y comportamiento

- Contraseñas nuevas con **Argon2id**; los hashes `scrypt` del frontend anterior se validan una única vez y se actualizan automáticamente al iniciar sesión.
- Sesiones opacas, aleatorias, persistidas en MongoDB, con cookies `HttpOnly`, `SameSite=Lax` y expiración de 365 días. Un cambio de contraseña revoca todas las sesiones de ese usuario.
- Autorización por propietario en cada recurso; los administradores gestionan usuarios pero nunca se eliminan ni restablecen mediante la API.
- Validación en el servidor de IDs, relaciones de pertenencia, fechas, pesos/repeticiones, tamaños de payload y límites de colecciones.
- Protección CSRF para todas las solicitudes que modifican estado: exige `Origin` igual a `FRONTEND_ORIGIN`. El frontend SvelteKit lo envía en sus llamadas servidor-a-servidor.
- Índices de MongoDB y limpieza de datos/sesiones al eliminar una cuenta.

## Variables de entorno

Parte de `.env.example`. En producción son obligatorias `MONGODB_URI`, `ADMIN_PASSWORD` y una `FRONTEND_ORIGIN` HTTPS; use `RUST_ENV=production` (o `NODE_ENV=production`).

## Desarrollo

```bash
cp .env.example .env
cargo run
```

Por defecto escucha en `http://localhost:8080`. El frontend debe configurar `API_URL=http://localhost:8080`.

## Despliegue en Coolify

El repositorio incluye un `Dockerfile` de producción para Coolify. Crea una aplicación Dockerfile apuntando a este proyecto (o usa `gym-tracker-api` como directorio base si ambos proyectos están en un monorepo) y configura el puerto interno **8080**. La imagen ejecuta como usuario sin privilegios, usa `RUST_ENV=production` y publica `GET /health` para health checks.

En las variables de entorno de Coolify define:

| Variable | Valor de producción |
| --- | --- |
| `MONGODB_URI` | URI privada de MongoDB/Atlas. |
| `MONGODB_DB` | Opcional; por defecto `gym_tracker`. |
| `ADMIN_USERNAME` | Opcional; por defecto `alexioficial`. |
| `ADMIN_PASSWORD` | Contraseña inicial/autoritaria del administrador; obligatoria. |
| `FRONTEND_ORIGIN` | URL HTTPS pública exacta de `gym-tracker`, por ejemplo `https://gym.example.com`. |
| `PORT` | `8080` (o el puerto interno seleccionado en Coolify). |

No configures `SESSION_COOKIE_SECURE=false` en producción: la API lo fuerza a seguro. En el frontend configura `API_URL` con la URL interna que Coolify expone para este servicio, y conserva `ORIGIN` como su URL pública HTTPS.
