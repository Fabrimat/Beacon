# Security Policy

## Reporting a vulnerability

Do not open a public issue. Report privately through GitHub security
advisories: the repository's **Security** tab, then **Report a vulnerability**.

## Scope

The JavaScript host sandbox is capability-restricted. It limits what add-on
scripts can reach, but it is not a hostile-code-proof isolation boundary. Treat
running untrusted scripts as still carrying risk. See
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) sections 5.4 and 5.5.
