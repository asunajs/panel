# host_api/files.py —— 面板内文件系统 API 的 **CPython 侧等价实现**。
#
# 面板内的 Python 插件没有 `open()` / `os`（嵌入式宿主是 PocketPy，不带这些），
# 所以宿主在 `host_api` 门面上给了一套窄接口：`read_text` / `write_text` /
# `data_dir` … 这个文件是它在 CPython 下的同名实现 —— **没有它，
# 「同一份插件面板内外都能跑」这句话对文件访问就不成立**。
#
# ## 两边的一致与不一致
#
# | | 面板内（PocketPy 宿主） | 面板外（这里，CPython） |
# | --- | --- | --- |
# | 落点 | `<PLUGIN_DATA_DIR>/<插件名>/` | 进程 cwd |
# | 越界 | `OSError` + `EACCES`（拒绝，不夹回） | 没有围栏（原生文件系统） |
# | 异常 | `OSError`，宿主给的 Node 风格码 | `OSError`，`.code` 由 `errno` 映射 |
#
# **落点不同是刻意的**：面板里必须有个围栏（插件是长期在跑的别人写的代码），
# 而面板外你有的是正常的文件系统权限，再加一层围栏只是让人困惑。可移植的写法
# 是**只用相对路径**，两边都落在自己的"当前目录"下。
#
# ⚠ 用 `.code` 而不是 `.errno`：宿主侧给的码是 Node 风格的名字（`ENOENT` /
# `EACCES` / `EISDIR`…）。两边都让插件"认码不认类型"，同一段 `except OSError`
# 就不用按宿主分叉。

import errno as _errno
import json as _json
import os as _os
import shutil as _shutil

#: `errno` → Node 风格码。宿主侧的码就是这个命名，两边对齐才有意义。
#: 只列插件真会碰到的那些，其余落到 `EIO`（与宿主侧的兜底一致）。
_ERRNO_TO_CODE = {
    _errno.ENOENT: "ENOENT",
    _errno.EACCES: "EACCES",
    _errno.EPERM: "EACCES",
    _errno.EEXIST: "EEXIST",
    _errno.EINVAL: "EINVAL",
    _errno.EISDIR: "EISDIR",
    _errno.ENOTDIR: "ENOTDIR",
    _errno.ENOTEMPTY: "ENOTEMPTY",
    _errno.EFBIG: "EFBIG",
    _errno.ENAMETOOLONG: "ENAMETOOLONG",
}


def _code_for(exc):
    """从 `OSError` 里取一个 Node 风格的码。"""
    return _ERRNO_TO_CODE.get(getattr(exc, "errno", None), "EIO")


def _wrap(exc):
    """把原生异常补上 `.code`、并把码放到消息开头，再原样抛出。

    两个动作各有理由：

    * **`.code`**：宿主侧给的码是 Node 风格的名字（`ENOENT` / `EACCES`…），
      两边都让插件"认码不认类型"，同一段 `except OSError` 不用按宿主分叉。
    * **消息以码开头**：面板侧**挂不了属性**（PocketPy 的 `OSError` 既不许挂
      属性也不许当基类，实测），所以两边共同的约定只能是消息前缀。
      于是 `getattr(e, "code", str(e).split(":")[0])` 在两个宿主里都对。

    类型不动（`FileNotFoundError` 还是它）—— 插件里 `except FileNotFoundError`
    这种写法不该因为换宿主就失效。
    """
    if not getattr(exc, "code", None):
        code = _code_for(exc)
        try:
            exc.code = code
        except (AttributeError, TypeError):  # 个别异常不许挂属性
            pass
        try:
            exc.args = ("{}: {}".format(code, exc),)
        except (AttributeError, TypeError):
            pass
    raise exc


def _check_name(name):
    if not isinstance(name, str):
        raise TypeError("路径必须是字符串，收到 " + type(name).__name__)
    return name


def data_dir():
    """面板里是插件自己的数据目录；这里就是进程的工作目录。

    （排查时很有用 —— 同一份插件在两处的「我到底写哪去了」答案不同，
    这是设计如此，不是 bug。见本文件头部那张表。）
    """
    return _os.getcwd()


def read_text(name):
    _check_name(name)
    try:
        with open(name, encoding="utf-8") as handle:
            return handle.read()
    except OSError as exc:
        _wrap(exc)


def read_bytes(name):
    _check_name(name)
    try:
        with open(name, "rb") as handle:
            return handle.read()
    except OSError as exc:
        _wrap(exc)


def write_text(name, text):
    _check_name(name)
    if not isinstance(text, str):
        raise TypeError("write_text(name, text): text 必须是字符串")
    try:
        with open(name, "w", encoding="utf-8") as handle:
            handle.write(text)
    except OSError as exc:
        _wrap(exc)
    return None


def append_text(name, text):
    _check_name(name)
    if not isinstance(text, str):
        raise TypeError("append_text(name, text): text 必须是字符串")
    try:
        with open(name, "a", encoding="utf-8") as handle:
            handle.write(text)
    except OSError as exc:
        _wrap(exc)
    return None


def write_bytes(name, data):
    _check_name(name)
    try:
        with open(name, "wb") as handle:
            handle.write(bytes(data))
    except OSError as exc:
        _wrap(exc)
    return None


def exists(name):
    _check_name(name)
    return _os.path.exists(name)


def list_dir(name):
    _check_name(name)
    try:
        return sorted(_os.listdir(name))
    except OSError as exc:
        _wrap(exc)


def mkdir(name):
    _check_name(name)
    try:
        _os.makedirs(name, exist_ok=True)
    except OSError as exc:
        _wrap(exc)
    return None


def remove(name):
    """尽力删（文件或整棵目录），与宿主侧同语义。

    文件不存在**不报错** —— 宿主侧的 `remove` 走的是 `force` 那条路，
    两边必须一样，否则"删之前先判断存不存在"的代码在有围栏那边反而多余。
    """
    _check_name(name)
    try:
        if _os.path.isdir(name):
            _shutil.rmtree(name, ignore_errors=True)
        else:
            _os.remove(name)
    except FileNotFoundError:
        return None
    except OSError as exc:
        _wrap(exc)
    return None


def rename(from_name, to_name):
    _check_name(from_name)
    _check_name(to_name)
    try:
        _os.replace(from_name, to_name)
    except OSError as exc:
        _wrap(exc)
    return None


def stat(name):
    _check_name(name)
    try:
        st = _os.stat(name)
    except OSError as exc:
        _wrap(exc)
    return {
        "size": st.st_size,
        "is_dir": _os.path.isdir(name),
        "is_file": _os.path.isfile(name),
        "mtime_ms": int(st.st_mtime * 1000),
    }
