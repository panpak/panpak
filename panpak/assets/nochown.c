/* SPDX-License-Identifier: MIT OR Apache-2.0
 *
 * A preload library for the build sandbox, free of any other library (no libc: it is built with -nostdlib).
 *
 * The sandbox is a user namespace with ONE user id. `chown` to another id fails there with EINVAL, and `dpkg` stops at the first file that
 * a package wants to give to another owner. Here every `chown` reports success; the files stay owned by the one id. Only the build
 * sandbox loads it. */
typedef unsigned int uid_t;
typedef unsigned int gid_t;
int chown(const char *p, uid_t u, gid_t g) { (void)p; (void)u; (void)g; return 0; }
int lchown(const char *p, uid_t u, gid_t g) { (void)p; (void)u; (void)g; return 0; }
int fchown(int fd, uid_t u, gid_t g) { (void)fd; (void)u; (void)g; return 0; }
int fchownat(int d, const char *p, uid_t u, gid_t g, int f) { (void)d; (void)p; (void)u; (void)g; (void)f; return 0; }
