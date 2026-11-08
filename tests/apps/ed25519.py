# SPDX-License-Identifier: MIT OR Apache-2.0
"""Ed25519 signing (RFC 8032), in plain Python, so that the harness can sign a manifest again without a package from outside the standard library.

Only signing and the public key are here, and only the harness uses them (a package made by `panpak convert` gets extra permissions and is signed again with the
harness's own key). It is slow (a few milliseconds) and not constant-time: it is a test tool, never for a key that matters. The tests check it against the vectors of
RFC 8032 section 7.1 and against `panpak` itself (the package verifies at install)."""
import hashlib

P = 2**255 - 19
Q = 2**252 + 27742317777372353535851937790883648493
D = -121665 * pow(121666, P - 2, P) % P
I = pow(2, (P - 1) // 4, P)


def _inv(x):
    return pow(x, P - 2, P)


def _xrecover(y):
    xx = (y * y - 1) * _inv(D * y * y + 1)
    x = pow(xx, (P + 3) // 8, P)
    if (x * x - xx) % P != 0:
        x = (x * I) % P
    return P - x if x % 2 != 0 else x


BY = 4 * _inv(5) % P
B = (_xrecover(BY), BY, 1, _xrecover(BY) * BY % P)


def _add(a, b):
    """Extended homogeneous coordinates (X, Y, Z, T)."""
    x1, y1, z1, t1 = a
    x2, y2, z2, t2 = b
    A = (y1 - x1) * (y2 - x2) % P
    Bb = (y1 + x1) * (y2 + x2) % P
    C = 2 * t1 * t2 * D % P
    Dd = 2 * z1 * z2 % P
    E, F, G, H = Bb - A, Dd - C, Dd + C, Bb + A
    return (E * F % P, G * H % P, F * G % P, E * H % P)


def _mul(s, point):
    r = (0, 1, 1, 0)
    while s:
        if s & 1:
            r = _add(r, point)
        point = _add(point, point)
        s >>= 1
    return r


def _encode(point):
    x, y, z, _ = point
    zi = _inv(z)
    x, y = x * zi % P, y * zi % P
    return (y | ((x & 1) << 255)).to_bytes(32, "little")


def _h(m):
    return hashlib.sha512(m).digest()


def _clamp(h):
    a = int.from_bytes(h[:32], "little")
    a &= (1 << 254) - 8
    return a | (1 << 254)


def public_key(seed):
    return _encode(_mul(_clamp(_h(seed)), B))


def sign(seed, message):
    """The 64-byte signature of `message` with the 32-byte secret `seed`."""
    if len(seed) != 32:
        raise ValueError("the secret key is 32 bytes")
    h = _h(seed)
    a = _clamp(h)
    pk = _encode(_mul(a, B))
    r = int.from_bytes(_h(h[32:] + message), "little") % Q
    big_r = _encode(_mul(r, B))
    k = int.from_bytes(_h(big_r + pk + message), "little") % Q
    s = (r + k * a) % Q
    return big_r + s.to_bytes(32, "little")
