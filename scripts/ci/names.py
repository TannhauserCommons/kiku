"""names: the files a release holds and the text that says which is which."""
ARCHS = {
    "x64": ("Kiku-setup.exe", "Windows 10 and 11 on x64, most PCs"),
    "arm64": ("Kiku-arm64-setup.exe", "Windows 11 on ARM, such as Snapdragon laptops"),
}
STABLE_TARBALL = "kiku-engine-linux-x64.tar.gz"
LINUX_FOR = "Linux model host (x64), to share models from a Linux PC or server (Ubuntu 22.04 or Debian 12 and newer; HOSTING.md inside says how)"


def installer(version, arch):
    """The installer's name: Kiku-<version>-<arch>-setup.exe."""
    return f"Kiku-{version}-{arch}-setup.exe"


def tarball(version):
    """The Linux tarball's name."""
    return f"kiku-engine-{version}-linux-x64.tar.gz"


def image(repo):
    """The Docker image of a repository (ghcr.io wants the owner in lower case)."""
    return f"ghcr.io/{repo.split('/')[0].lower()}/kiku-engine"


def downloads(repo, version):
    """The release text's "Downloads" list: which file is for whom."""
    base = f"https://github.com/{repo}/releases/download/v{version}"
    lines = [f"- {who}: [{installer(version, arch)}]({base}/{installer(version, arch)})" for arch, (_, who) in ARCHS.items()]
    lines.append(f"- {LINUX_FOR}: [{tarball(version)}]({base}/{tarball(version)}), or the Docker image `{image(repo)}`")
    return "### Downloads\n\n" + "\n".join(lines) + "\n"
