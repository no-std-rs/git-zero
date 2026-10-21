Public SHA-1 collision regression vectors from sha1collisiondetection 0.3.4
(https://gitlab.com/sequoia-pgp/sha1collisiondetection), under the included
MIT license. The SHAttered prefix is the first 320 bytes of shattered-1.pdf.

Native Git 2.56 detects all three raw messages as collision attacks, while
accepting their correctly prefixed canonical blob hashes in both formats.
The tests preserve that distinction: authenticate the actual hashed stream,
including its Git header, rather than scanning or blacklisting blob bytes.
