// 调试签名材料由 signing/ 维护，密钥库密码从 macOS Keychain 读取。
const fs = require('node:fs');
const path = require('node:path');
const crypto = require('node:crypto');
const { spawnSync } = require('node:child_process');

const root = path.resolve(__dirname, '..');
const local = path.join(root, '.signing');
const signed = path.join(root, 'entry/build/default/outputs/default/entry-default-signed.hap');
let password = '';

function checked(command, args, options = {}) {
  const result = spawnSync(command, args, { encoding: 'utf8', ...options });
  if (result.error || result.status !== 0) {
    throw new Error(result.error?.message || result.stderr?.trim() || '签名命令执行失败。');
  }
  return result.stdout;
}

function main() {
  fs.rmSync(signed, { force: true });
  const configFile = path.join(root, 'signing/config.json');
  if (!fs.existsSync(configFile)) {
    throw new Error('缺少 signing/config.json。');
  }
  const deveco = process.argv[2];
  if (!deveco) throw new Error('请通过 make build 执行构建。');
  const config = JSON.parse(fs.readFileSync(configFile, 'utf8'));
  const certificate = path.resolve(root, config.certificate);
  const profile = path.resolve(root, config.profile);
  const store = path.resolve(root, config.storeFile);
  fs.mkdirSync(local, { recursive: true, mode: 0o700 });
  const chain = fs.readFileSync(certificate, 'utf8').match(/-----BEGIN CERTIFICATE-----[\s\S]*?-----END CERTIFICATE-----/g);
  if (!chain?.length) throw new Error('调试证书链为空。');
  const leaf = new crypto.X509Certificate(chain[chain.length - 1]);
  const now = Date.now();
  if (now < Date.parse(leaf.validFrom) || now >= Date.parse(leaf.validTo)) {
    throw new Error('调试证书尚未生效或已过期。');
  }
  const provision = JSON.parse(checked('openssl',
    ['cms', '-verify', '-inform', 'DER', '-in', profile, '-noverify']));
  const app = JSON.parse(fs.readFileSync(path.join(root, 'AppScope/app.json5'), 'utf8'));
  if (provision.type !== 'debug' || provision['bundle-info']['bundle-name'] !== app.app.bundleName) {
    throw new Error('调试 Profile 的类型或包名与应用不匹配。');
  }
  if (now / 1000 < provision.validity['not-before'] || now / 1000 >= provision.validity['not-after']) {
    throw new Error('调试 Profile 尚未生效或已过期。');
  }
  const profileCert = new crypto.X509Certificate(provision['bundle-info']['development-certificate']);
  if (!leaf.publicKey.equals(profileCert.publicKey)) throw new Error('证书与 Profile 不配套。');
  password = checked('/usr/bin/security', ['find-generic-password',
    '-s', config.keychainService, '-a', config.keychainAccount, '-w']).replace(/\n$/, '');
  const java = path.join(deveco, 'jbr/Contents/Home/bin');
  const exported = checked(path.join(java, 'keytool'), ['-exportcert', '-rfc',
    '-alias', config.keyAlias, '-keystore', store, '-storetype', 'PKCS12',
    '-storepass:env', 'LOKI_SIGNING_PASSWORD'], { env: { ...process.env, LOKI_SIGNING_PASSWORD: password } });
  if (!leaf.publicKey.equals(new crypto.X509Certificate(exported).publicKey)) {
    throw new Error('密钥库与调试证书不匹配。');
  }
  const jar = path.join(deveco, 'sdk/default/openharmony/toolchains/lib/hap-sign-tool.jar');
  checked(path.join(java, 'javac'), ['-encoding', 'UTF-8', '-cp', jar, '-d', local,
    path.join(__dirname, 'LokiHapSigner.java')]);
  const build = JSON.parse(fs.readFileSync(path.join(root, 'build-profile.json5'), 'utf8'));
  const version = String(build.app.products.find(p => p.name === 'default').compatibleSdkVersion);
  const compatible = /^\d+$/.test(version) ? version : version.match(/\((\d+)\)/)?.[1];
  if (!compatible) throw new Error('无法确定签名兼容 API 版本。');
  const payload = Buffer.from(password);
  const length = Buffer.alloc(4);
  length.writeUInt32BE(payload.length);
  const result = checked(path.join(java, 'java'), ['-cp', `${local}:${jar}`, 'LokiHapSigner',
    'sign-app', '-mode', 'localSign', '-keyAlias', config.keyAlias,
    '-appCertFile', certificate, '-profileFile', profile, '-keystoreFile', store,
    '-inFile', signed.replace('-signed.hap', '-unsigned.hap'), '-outFile', signed,
    '-signAlg', 'SHA256withECDSA', '-compatibleVersion', compatible],
  { input: Buffer.concat([length, payload]) });
  if (result) process.stdout.write(result);
  if (!fs.existsSync(signed)) throw new Error('未生成已签名 HAP。');
  console.log('>>> 已生成调试签名 HAP。');
}

try {
  main();
} catch (error) {
  fs.rmSync(signed, { force: true });
  const message = password ? error.message.replaceAll(password, '[密码已隐藏]') : error.message;
  console.error(`签名失败：${message}`);
  process.exitCode = 1;
}
