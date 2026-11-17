# Fixed native KTU2 functions. Import/definition only; no top-level namespace IO.
import ctypes
import errno
import hashlib
import os
import pwd
import re
import stat
import sys
import time

LIMIT = 268435456
TARGET = b'tune-agent'
LOCK = b'.tune-agent-update.lock'

class Refused(Exception):
    def __init__(self, reason='Unsafe'):
        self.reason = reason

class CloseUncertain(Refused):
    pass

def uint(text, bits=64):
    if not re.fullmatch(r'0|[1-9][0-9]*', text):
        raise Refused()
    value = int(text)
    if value >= 1 << bits:
        raise Refused()
    return value

def signed(text):
    if not re.fullmatch(r'0|-?[1-9][0-9]*', text):
        raise Refused()
    value = int(text)
    if not -(1 << 127) <= value < (1 << 127):
        raise Refused()
    return value

def digest(text):
    if re.fullmatch(r'[0-9a-f]{64}', text) is None:
        raise Refused()
    return text

def home_components(home):
    if not isinstance(home, bytes) or not 1 <= len(home) <= 384 or not home.startswith(b'/') or b'\0' in home:
        raise Refused('Account')
    parts = home[1:].split(b'/')
    if any(p in (b'', b'.', b'..') for p in parts) or len(parts) + 3 > 24:
        raise Refused('Account')
    return parts

def validate_account(uid, euid, pw_home, env_home, platform, machine):
    if platform != 'darwin' or machine != 'arm64':
        raise Refused('Unsupported')
    if uid != euid or pw_home != env_home:
        raise Refused('Account')
    home_components(pw_home)
    return {'uid': uid, 'home': pw_home}

def production_account():
    uid = os.getuid()
    return validate_account(uid, os.geteuid(), os.fsencode(pwd.getpwuid(uid).pw_dir), os.environb.get(b'HOME'), sys.platform, os.uname().machine)

def parse_record(record):
    if not record.isascii() or len(record) > 3072:
        raise Refused()
    f = record.split('|')
    if len(f) != 7 or f[2:4] != ['darwin', 'arm64']:
        raise Refused()
    uid, lease = uint(f[0], 32), uint(f[4])
    if re.fullmatch(r'(?:[0-9a-f]{2}){1,384}', f[1]) is None:
        raise Refused()
    home = bytes.fromhex(f[1])
    parts = home_components(home)
    parents = []
    for p in f[5].split(';'):
        a = p.split(',')
        if len(a) != 4:
            raise Refused()
        parents.append(tuple(uint(v, 64 if i < 2 else 32) for i, v in enumerate(a)))
    if len(parents) != len(parts) + 3:
        raise Refused()
    leaf = None
    if f[6] != 'M':
        a = f[6].split(',')
        if len(a) != 10 or a[0] != 'P':
            raise Refused()
        leaf = (uint(a[1]), uint(a[2]), uint(a[3], 32), uint(a[4]), uint(a[5], 32), signed(a[6]), signed(a[7]), uint(a[8]), digest(a[9]))
        if not 1 <= leaf[3] <= LIMIT or leaf[7] != 1:
            raise Refused()
    return {'uid': uid, 'home': home, 'lease': lease, 'parents': parents, 'leaf': leaf}

def parse_header(raw, body_len):
    try:
        text = raw.decode('ascii')
    except UnicodeError:
        raise Refused() from None
    names = ['token', 'version', 'target', 'manifest', 'size', 'sha256', 'context', 'mode', 'source-size', 'source-sha256', 'ttl-ms', 'observation']
    lines = text.split('\n')
    if len(lines) != 13 or lines[-1] != '':
        raise Refused()
    result = {}
    for name, line in zip(names, lines):
        if not line.startswith(name + '='):
            raise Refused()
        result[name] = line[len(name) + 1:]
    h = result
    if re.fullmatch(r'[A-Za-z0-9_-]{1,64}', h['token']) is None:
        raise Refused()
    for field in ('version', 'target'):
        if re.fullmatch(r'[A-Za-z0-9._-]{1,128}', h[field]) is None:
            raise Refused()
    if h['target'] != 'aarch64-apple-darwin':
        raise Refused('Unsupported')
    for field in ('manifest', 'sha256', 'context', 'source-sha256'):
        digest(h[field])
    h['size'] = uint(h['size'])
    h['source-size'] = uint(h['source-size'])
    h['ttl-ms'] = uint(h['ttl-ms'])
    if h['size'] != body_len or body_len > LIMIT or not 1 <= h['source-size'] <= LIMIT or not 1 <= h['ttl-ms'] <= 120000:
        raise Refused()
    if h['mode'] == 'inspect':
        if body_len != 0 or h['sha256'] != hashlib.sha256(b'').hexdigest() or h['observation'] != '-':
            raise Refused()
    elif h['mode'] == 'place':
        if body_len != h['source-size'] or h['sha256'] != h['source-sha256']:
            raise Refused()
        h['expected'] = parse_record(h['observation'])
    else:
        raise Refused()
    return h

class Syscalls:
    def __init__(self):
        self.uncertain = False
    def close(self, fd, label):
        os.close(fd)
    def account(self):
        return production_account()
    def write(self, fd, data):
        return os.write(fd, data)
    def fsync(self, fd):
        os.fsync(fd)
    def point(self, name, chain):
        pass
    def rename(self, fd, source, target):
        lib = ctypes.CDLL('/usr/lib/libSystem.B.dylib', use_errno=True)
        fn = lib.renameatx_np
        fn.argtypes = [ctypes.c_int, ctypes.c_char_p, ctypes.c_int, ctypes.c_char_p, ctypes.c_uint]
        fn.restype = ctypes.c_int
        if fn(fd, source, fd, target, 4) != 0:  # RENAME_EXCL
            code = ctypes.get_errno()
            raise OSError(code, 'exclusive rename failed')

def close_once(ops, fd, label):
    # Ownership is consumed even on EINTR; a failed close never licenses reuse.
    try:
        ops.close(fd, label)
    except OSError:
        ops.uncertain = True
        raise CloseUncertain('Io') from None

def parent_state(s):
    return (s.st_dev, s.st_ino, s.st_uid, s.st_mode)

def leaf_state(s):
    return (s.st_dev, s.st_ino, s.st_uid, s.st_size, s.st_mode, s.st_mtime_ns, s.st_ctime_ns, s.st_nlink)

def directory(s, account, owned):
    if not stat.S_ISDIR(s.st_mode) or s.st_uid not in (0, account['uid']) or s.st_mode & 0o022 or (owned and s.st_uid != account['uid']):
        raise Refused()

class Chain:
    def __init__(self, account, ops):
        self.account, self.ops, self.fds, self.names = account, ops, [], []
    @property
    def bin(self):
        return self.fds[-1]
    def verify(self):
        if len(self.fds) != len(self.names) or any(fd is None for fd in self.fds):
            raise Refused('Io')
        result = []
        for i, fd in enumerate(self.fds):
            held = os.fstat(fd)
            named = os.stat(b'/' if i == 0 else self.names[i], dir_fd=None if i == 0 else self.fds[i-1], follow_symlinks=False)
            directory(held, self.account, i >= len(self.fds) - 3)
            if parent_state(held) != parent_state(named):
                raise Refused()
            result.append(parent_state(held))
        return result
    def close_index(self, i, label):
        fd, self.fds[i] = self.fds[i], None
        if fd is not None:
            close_once(self.ops, fd, label)
    def close_all(self):
        failed = False
        for i in range(len(self.fds)-1, -1, -1):
            try:
                self.close_index(i, 'bin' if i == len(self.fds)-1 else 'parent')
            except CloseUncertain:
                failed = True
        if failed:
            raise CloseUncertain('Io')

def open_canonical(account, ops):
    chain = Chain(account, ops)
    parts = [b'/'] + home_components(account['home']) + [b'.katala-tune', b'bin']
    try:
        for i, name in enumerate(parts):
            fd = os.open(name, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC, dir_fd=None if i == 0 else chain.fds[-1])
            chain.fds.append(fd)
            chain.names.append(name)
        chain.verify()
        return chain
    except BaseException:
        chain.close_all()
        raise

def read_leaf(chain, name=TARGET):
    fd = None
    try:
        try:
            named = os.stat(name, dir_fd=chain.bin, follow_symlinks=False)
        except FileNotFoundError:
            return None
        s = named
        if not stat.S_ISREG(s.st_mode) or s.st_nlink != 1 or s.st_uid != chain.account['uid'] or s.st_mode & 0o6022 or s.st_mode & 0o111 != 0o111 or not 1 <= s.st_size <= LIMIT:
            raise Refused()
        fd = os.open(name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC, dir_fd=chain.bin)
        before = leaf_state(os.fstat(fd))
        if before != leaf_state(named):
            raise Refused()
        h, count = hashlib.sha256(), 0
        while True:
            data = os.read(fd, 65536)
            if not data:
                break
            count += len(data)
            if count > LIMIT:
                raise Refused()
            h.update(data)
        if before != leaf_state(os.fstat(fd)) or before != leaf_state(os.stat(name, dir_fd=chain.bin, follow_symlinks=False)) or count != before[3]:
            raise Refused()
        return before + (h.hexdigest(),)
    finally:
        if fd is not None:
            close_once(chain.ops, fd, 'read')

def record(account, parents, leaf, lease):
    leaf_text = 'M' if leaf is None else 'P,' + ','.join(map(str, leaf))
    text = '|'.join([str(account['uid']), account['home'].hex(), 'darwin', 'arm64', str(lease), ';'.join(','.join(map(str, p)) for p in parents), leaf_text])
    parse_record(text)
    return text

def inspect_at(chain, account, source_meta, lease_ns):
    try:
        parents = chain.verify()
        leaf = read_leaf(chain)
        if parents != chain.verify():
            raise Refused()
        result = record(account, parents, leaf, lease_ns)
        chain.close_all()
        if time.monotonic_ns() >= lease_ns:
            raise Refused('Expired')
        return result
    finally:
        chain.close_all()

def equal_after_rename(actual, before):
    # rename can advance ctime. All other state/hash must still be exact.
    return actual is not None and actual[:6] == before[:6] and actual[7:] == before[7:]

def named_owned(chain, name, identity):
    try:
        s = os.stat(name, dir_fd=chain.bin, follow_symlinks=False)
    except FileNotFoundError:
        return False
    if not stat.S_ISREG(s.st_mode) or s.st_nlink != 1 or s.st_uid != chain.account['uid'] or (s.st_dev, s.st_ino) != identity:
        raise Refused()
    return True

def body_to_stage(chain, stream, size, expected_hash, fd):
    h, left = hashlib.sha256(), size
    while left:
        data = stream.read(min(left, 65536))
        if not data:
            raise Refused('Io')
        left -= len(data)
        h.update(data)
        if fd is not None:
            view = memoryview(data)
            while view:
                n = chain.ops.write(fd, view)
                if n <= 0 or n > len(view):
                    raise Refused('Io')
                view = view[n:]
    if stream.read(1) or h.hexdigest() != expected_hash:
        raise Refused()

def finish(chain, lock_fd, lock_identity):
    # Keep bin until all other checked closes and lock identity/empty checks.
    for i in range(len(chain.fds)-2, -1, -1):
        chain.close_index(i, 'parent')
    fd, lock_fd[0] = lock_fd[0], None
    if fd is not None:
        close_once(chain.ops, fd, 'lock')
    s = os.stat(LOCK, dir_fd=chain.bin, follow_symlinks=False)
    if parent_state(s) != lock_identity or not stat.S_ISDIR(s.st_mode):
        raise Refused()
    os.rmdir(LOCK, dir_fd=chain.bin)  # fails rather than deleting unknown entries
    chain.close_index(len(chain.fds)-1, 'bin')

def place_at(chain, account, expected, source_meta, token, stream, body_len):
    ops = chain.ops
    old = expected['leaf']
    old_hash = '-' if old is None else old[-1]
    def frame(event, backup=False, rollback=False):
        print('KTU2', token, source_meta['context'], event, source_meta['source-sha256'], old_hash, int(backup), int(rollback), flush=True)
    if expected['uid'] != account['uid'] or expected['home'] != account['home'] or time.monotonic_ns() >= expected['lease']:
        raise Refused('Expired')
    parents = chain.verify()
    if parents != expected['parents'] or read_leaf(chain) != old:
        raise Refused()
    frame('Started')
    lock_fd, lock_identity = [None], None
    stage = b'.tune-agent-stage-' + token.encode('ascii')
    backup = b'.tune-agent-backup-' + token.encode('ascii')
    stage_fd, stage_id, staged = None, None, None
    backed, final, backup_verified, rollback = False, False, False, False
    backup_state, final_state = None, None
    try:
        ops.point('before_lock', chain)
        os.mkdir(LOCK, 0o700, dir_fd=chain.bin)
        lock_identity = parent_state(os.stat(LOCK, dir_fd=chain.bin, follow_symlinks=False))
        lock_fd[0] = os.open(LOCK, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC, dir_fd=chain.bin)
        if parent_state(os.fstat(lock_fd[0])) != lock_identity or lock_identity[2] != account['uid'] or lock_identity[3] & 0o7777 != 0o700:
            raise Refused()
        if chain.verify() != parents or read_leaf(chain) != old:
            raise Refused()
        ops.point('after_lock', chain)
        if old is not None and old[-1] == source_meta['source-sha256']:
            body_to_stage(chain, stream, body_len, source_meta['source-sha256'], None)
            if chain.verify() != parents or read_leaf(chain) != old or ops.account() != account:
                raise Refused()
            finish(chain, lock_fd, lock_identity)
            frame('Unchanged')
            return
        stage_fd = os.open(stage, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_NONBLOCK | os.O_CLOEXEC, 0o600, dir_fd=chain.bin)
        initial = os.fstat(stage_fd)
        stage_id = (initial.st_dev, initial.st_ino)
        if not stat.S_ISREG(initial.st_mode) or initial.st_size != 0 or initial.st_nlink != 1 or initial.st_uid != account['uid'] or initial.st_mode & 0o7777 != 0o600:
            raise Refused()
        body_to_stage(chain, stream, body_len, source_meta['source-sha256'], stage_fd)
        os.fchmod(stage_fd, 0o755)
        ops.fsync(stage_fd)
        fd, stage_fd = stage_fd, None
        close_once(ops, fd, 'stage')
        staged = read_leaf(chain, stage)
        if staged is None or staged[:2] != stage_id or staged[3] != body_len or staged[4] & 0o7777 != 0o755 or staged[-1] != source_meta['source-sha256']:
            raise Refused()
        frame('Staged')
        ops.point('before_commit', chain)
        if ops.account() != account or chain.verify() != parents or read_leaf(chain) != old or read_leaf(chain, stage) != staged:
            raise Refused()
        if old is not None:
            ops.rename(chain.bin, TARGET, backup)
            backed = True
            # Once the rename succeeded rollback owns this name only if exactold
            # was actually re-observed; never adopt a preexisting backup.
            backup_verified = False
            backup_state = read_leaf(chain, backup)
            backup_verified = equal_after_rename(backup_state, old)
            if not backup_verified:
                raise Refused()
            frame('BackedUp', True)
            ops.point('after_backup', chain)
        ops.rename(chain.bin, stage, TARGET)
        final = True
        ops.point('after_final_rename', chain)
        if chain.verify() != parents or not named_owned(chain, TARGET, stage_id):
            raise Refused()
        final_state = read_leaf(chain)
        if not equal_after_rename(final_state, staged):
            raise Refused()
        if old is not None:
            backup_verified = False
            backup_verified = backup_state is not None and read_leaf(chain, backup) == backup_state
            if not backup_verified:
                raise Refused()
        ops.point('final_verify', chain)
        # Re-observe after the checkpoint as well; an unowned occupant is never
        # hashed by recovery unless its already-held stage identity matches.
        if chain.verify() != parents or not named_owned(chain, TARGET, stage_id) or read_leaf(chain) != final_state:
            raise Refused()
        if old is not None:
            backup_verified = False
            backup_verified = backup_state is not None and read_leaf(chain, backup) == backup_state
            if not backup_verified:
                raise Refused()
        finish(chain, lock_fd, lock_identity)
        frame('Placed', backed)
        return
    except (OSError, Refused, AttributeError):
        # Finish-owned fd errors consume once before deciding whether any more
        # namespace mutation is permissible.
        if stage_fd is not None:
            fd, stage_fd = stage_fd, None
            try:
                close_once(ops, fd, 'stage')
            except CloseUncertain:
                pass
        if not ops.uncertain and chain.fds[-1] is not None:
            try:
                if chain.verify() != parents:
                    raise Refused()
                if backed:
                    backup_verified = False
                    backup_verified = backup_state is not None and read_leaf(chain, backup) == backup_state
                    if not backup_verified:
                        raise Refused()
                    try:
                        os.stat(TARGET, dir_fd=chain.bin, follow_symlinks=False)
                    except FileNotFoundError:
                        pass
                    else:
                        if not final or not named_owned(chain, TARGET, stage_id):
                            raise Refused()
                        os.unlink(TARGET, dir_fd=chain.bin)
                    ops.point('before_rollback', chain)
                    ops.rename(chain.bin, backup, TARGET)
                    backed, backup_verified = False, False
                    if not equal_after_rename(read_leaf(chain), old):
                        raise Refused()
                    ops.point('after_rollback', chain)
                    rollback = True
                elif final and old is None:
                    if named_owned(chain, TARGET, stage_id):
                        os.unlink(TARGET, dir_fd=chain.bin)
                    if read_leaf(chain) is not None:
                        raise Refused()
                    rollback = True
                if stage_id is not None and named_owned(chain, stage, stage_id):
                    ops.point('cleanup', chain)
                    os.unlink(stage, dir_fd=chain.bin)
                if lock_identity is not None:
                    finish(chain, lock_fd, lock_identity)
                else:
                    chain.close_all()
            except (OSError, Refused, AttributeError):
                rollback = False
                # Any uncertainty ends mutation. Owned descriptors still close.
        drain(stream)
        if lock_fd[0] is not None:
            fd, lock_fd[0] = lock_fd[0], None
            try:
                close_once(ops, fd, 'lock')
            except CloseUncertain:
                rollback = False
        try:
            chain.close_all()
        except CloseUncertain:
            rollback = False
        frame('Failed', backed and backup_verified and not rollback, rollback and not ops.uncertain)
        raise Refused('Io')
    finally:
        chain.close_all()

def drain(stream):
    while stream.read(65536):
        pass

def run_at(stream, header_bytes, body_len, account, ops):
    h, chain = None, None
    try:
        h = parse_header(header_bytes, body_len)
        lease = time.monotonic_ns() + h['ttl-ms'] * 1000000
        chain = open_canonical(account, ops)
        ops.point('opened', chain)
        if h['mode'] == 'inspect':
            if stream.read(1):
                raise Refused()
            observed = inspect_at(chain, account, h, lease)
            print('KTU2', h['token'], h['context'], 'Observed', h['source-sha256'], observed, flush=True)
        else:
            place_at(chain, account, h['expected'], h, h['token'], stream, body_len)
    except (OSError, Refused, AttributeError) as e:
        if chain is not None:
            try:
                chain.close_all()
            except Refused:
                pass
        drain(stream)
        if h is not None and h['mode'] == 'inspect':
            print('KTU2', h['token'], h['context'], 'InspectFailed', h['source-sha256'], e.reason if isinstance(e, Refused) else 'Io', flush=True)
        raise SystemExit(1)

def run_production(stream, header_bytes, body_len):
    try:
        account = production_account()
    except (OSError, KeyError, Refused):
        drain(stream)
        raise SystemExit(1)
    run_at(stream, header_bytes, body_len, account, Syscalls())
