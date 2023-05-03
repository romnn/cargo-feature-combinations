import * as core from "@actions/core";
import {
  parseCargoPackageManifestAsync,
  Repo,
  RustTarget,
} from "action-get-release";
import * as path from "path";

async function getVersion(): Promise<string> {
  let version = "latest";
  const manifest = await parseCargoPackageManifestAsync(
    path.join(__dirname, "../Cargo.toml")
  );
  let manifestVersion = manifest.package?.version;
  if (manifestVersion && manifestVersion !== "") {
    version = `v${manifestVersion}`;
  }
  let versionOverride = core.getInput("version");
  if (versionOverride && versionOverride !== "") {
    version = versionOverride;
  }
  return version;
}

async function run(): Promise<void> {
  const repo = new Repo();
  const version = await getVersion();
  core.debug(`version=${version}`);

  let release;
  try {
    release =
      version === "" || version === "latest"
        ? await repo.getLatestRelease()
        : await repo.getReleaseByTag(version);
  } catch (err: unknown) {
    throw new Error(
      `failed to fetch ${version} release for ${repo.fullName()}: ${err}`
    );
  }
  core.debug(
    `found ${
      release.assets().length
    } assets for ${version} release of ${repo.fullName()}`
  );

  const { platform, arch } = new RustTarget();
  core.debug(`host system: platform=${platform} arch=${arch}`);

  // cargo-fc-x86_64-unknown-linux-gnu.tar.gz
  const bin = "cargo-fc";
  const asset = `${bin}-action-${arch}-unknown-${platform}-gnu.tar.gz`;

  let downloaded;
  try {
    downloaded = await release.downloadAsset(asset, { cache: false });
  } catch (err: unknown) {
    throw new Error(`failed to download asset ${asset}: ${err}`);
  }

  core.addPath(downloaded);
  // const executable = path.join(downloaded, bin);
  // await exec.exec(executable);
}

run().catch((error) => core.setFailed(error.message));
