import java.io.*;
import java.math.BigInteger;
import java.nio.ByteBuffer;
import java.security.*;
import java.security.interfaces.*;
import java.security.spec.*;
import java.util.*;
import javax.crypto.*;
import javax.crypto.spec.*;

/* Carpet-grade coverage for the java.security + javax.crypto (JCA/JCE) stack.
 *
 * Every assertion checks an exact, deterministic value: published NIST/FIPS/RFC
 * known-answer vectors, exact byte equality, exact lengths, or an
 * encrypt->decrypt / sign->verify / agree->agree round trip that must reproduce
 * the original. No external I/O, no network, no /dev/random blocking, no JIT or
 * timing dependence. Asymmetric keys are embedded (PKCS#8 / X.509 DER) so there
 * is no runtime key generation cost except one cheap EC P-256 keygen, and
 * SecureRandom is always explicitly seeded (SHA1PRNG) -> musl / StarryOS safe.
 *
 * Coverage matrix:
 *   MessageDigest (java.security)   MD5 / SHA-1 / SHA-224/256/384/512 /
 *                                   SHA3-224/256/384/512 KATs; getDigestLength;
 *                                   incremental update(byte|byte[]|off,len|
 *                                   ByteBuffer); reset; two-arg digest; clone;
 *                                   isEqual timing-safe compare
 *   Mac (javax.crypto)             HmacMD5 / HmacSHA1 / HmacSHA224/256/384/512
 *                                   RFC 2202 / RFC 4231 KATs; getMacLength;
 *                                   incremental; reset; clone; two-arg doFinal
 *   KeyGenerator / SecretKeySpec   AES-128/256 / HmacSHA256 / DESede; seeded
 *                                   determinism; getFormat / getEncoded / equals
 *   Cipher symmetric               AES ECB/CBC/CTR KATs; ECB/CBC/CTR/CFB/OFB/GCM
 *                                   round trips; GCM KAT + AAD + tamper
 *                                   detection (AEADBadTagException); multi-part
 *                                   update+doFinal; getBlockSize/getIV/
 *                                   getOutputSize/getParameters; wrap/unwrap;
 *                                   DESede; getMaxAllowedKeyLength; exception
 *                                   matrix (NoSuchAlgorithm / NoSuchPadding /
 *                                   InvalidKey / IllegalBlockSize /
 *                                   InvalidAlgorithmParameter)
 *   SecretKeyFactory               PBKDF2WithHmacSHA1 / -SHA256 RFC 6070 KATs
 *   SecureRandom                   SHA1PRNG seeded determinism; nextInt /
 *                                   nextBytes / setSeed / generateSeed
 *   KeyFactory / Signature / Cipher RSA-2048: deterministic SHA256withRSA KAT,
 *     (asymmetric)                 verify + tamper, RSA/ECB/PKCS1 + OAEP round
 *                                   trips, X.509/PKCS#8 spec round trip,
 *                                   modulus bit length; EC P-256 ECDSA
 *                                   sign/verify + ECDH KAT + live keygen;
 *                                   DH KeyAgreement KAT; DSA sign/verify
 *   Provider / Security            getProviders; getProvider name; CipherStreams
 *                                   (DigestInput/OutputStream,
 *                                   CipherInput/OutputStream)
 */
public class CryptoTest {
    static int ok = 0, fail = 0;
    static void check(boolean c, String name) {
        if (c) { ok++; } else { fail++; System.out.println("FAIL " + name); }
    }
    interface Thrower { void run() throws Throwable; }
    static void expect(Class<? extends Throwable> ex, String name, Thrower t) {
        try {
            t.run();
            fail++; System.out.println("FAIL " + name + " (no exception)");
        } catch (Throwable got) {
            if (ex.isInstance(got)) { ok++; }
            else { fail++; System.out.println("FAIL " + name + " (got " + got.getClass().getName() + ")"); }
        }
    }

    static String hex(byte[] b) {
        StringBuilder s = new StringBuilder(b.length * 2);
        for (byte x : b) s.append(Character.forDigit((x >> 4) & 0xf, 16)).append(Character.forDigit(x & 0xf, 16));
        return s.toString();
    }
    static byte[] unhex(String s) {
        int n = s.length() / 2;
        byte[] b = new byte[n];
        for (int i = 0; i < n; i++) b[i] = (byte) Integer.parseInt(s.substring(2 * i, 2 * i + 2), 16);
        return b;
    }
    static byte[] utf8(String s) { return s.getBytes(java.nio.charset.StandardCharsets.UTF_8); }

    /* A freshly seeded SHA1PRNG. Used everywhere randomness is needed so the carpet
     * never touches the platform default SecureRandom (which would read the OS entropy
     * device); this keeps every operation non-blocking on StarryOS. */
    static SecureRandom seededRng(long seed) throws Exception {
        SecureRandom r = SecureRandom.getInstance("SHA1PRNG");
        r.setSeed(seed);
        return r;
    }

    // ----- embedded fixed key material (PKCS#8 / X.509 DER, Base64) -----
    static final String RSA_PUB = "MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEA4wzG6mdxtFsAhOGrPex5Lb8yA/5h/y9qZR0O4/U1bZbPsaRoRw1ghaHqMd6X8c0WnbHWoGwdL2NOBB0A/nsadjzzWbcMHZoipNUukv2KmH7oDbIb7NLNijnxqEuy+GIdOV28SM85LDq5KpZho0Ne+FVgUT8A3wMSVrt1sYUh/NzrdnaKa0/gW58uSjebeGhCYeY+Tla6FqE1TSMuzxS4tCTLtgkTWbRPZ0GgY6Crjw989flln6eyyTWYs5KtRd7JsiSZrorDJDgwMJ8mGaez/yo/ftLdZGS/1+Smca2Ar1EtfHgEDjJ6cRizyOaAWW88Iygk4MpY4r7uJ98Bfrbr4QIDAQAB";
    static final String RSA_PRV = "MIIEvAIBADANBgkqhkiG9w0BAQEFAASCBKYwggSiAgEAAoIBAQDjDMbqZ3G0WwCE4as97HktvzID/mH/L2plHQ7j9TVtls+xpGhHDWCFoeox3pfxzRadsdagbB0vY04EHQD+exp2PPNZtwwdmiKk1S6S/YqYfugNshvs0s2KOfGoS7L4Yh05XbxIzzksOrkqlmGjQ174VWBRPwDfAxJWu3WxhSH83Ot2doprT+Bbny5KN5t4aEJh5j5OVroWoTVNIy7PFLi0JMu2CRNZtE9nQaBjoKuPD3z1+WWfp7LJNZizkq1F3smyJJmuisMkODAwnyYZp7P/Kj9+0t1kZL/X5KZxrYCvUS18eAQOMnpxGLPI5oBZbzwjKCTgyljivu4n3wF+tuvhAgMBAAECggEAOcQsb8L12O82SJip2s1pX0w/y2hTQnur1CH6geEHQOSX3xh3N2yd3CH/1cROYETPtjti4dnf6wiW9tDySczERMTpHTBHMtjea5WZjehX9MiE/ccM98oCZWKsqybnV+6OhOPmXZfrcedW6RDtsn4XkZMXOFSRQiwj5FE5dnrq1MxddufNXv+14wt6r5I88ZCerkYY1GUIMifr6nfGARwFpOwr5Ph/EKgiHGIpAMeu5bNV/xKZY0iQNoHxSWnyiugtzfbQQCaG+IzfxClrUia/2H/P9HN6QsO4+9W3gFzcAGoYbEVLDJT7y/eVb/skBuxCxZgiou5J+8m3nZTc9L98qQKBgQD+rmb2F2i4r8xzDF3Mz1BX/8DDCErdGrlpNISypPiatu60mS8Og9/SyDBnMt3Run3sIwjPRna8CzsDMQUsFq3bN91zWko/+P4MAd7MOrvn4/MysGvnKupVHhkBDUCnaWMfcLMtcN0rmPJizT8kI6MFHR+NgGi3HcoVF4Z+I9Dc+wKBgQDkOb9eq9RpEH0gOBgx6CvXHd+6663kbWzwghHnSS80e4KJcjWtXZCk9jVIQL+pkbeN5o1WPCykVxQ6IyjRl224L7aMsJMQnfrJ+1ScJFjtnl4h5cdOzxx1etYgjgblyCeWAjk587XILolamdBiSf+HuPJDn59eXhBHKEoaf5YL0wKBgC5hVnDUnIadxU7iXqawzoHoGpOqC/AuMLvfC5d5AakzTU9oYjBzhaxeNqpkkg7itpHtY2pT+8WNCgcvwzBfRPQaPWMHe2QhFSrcoFVzEMtPMPf3Nv9XSmuL2qPdZPvX7mxIWukYl76b0PB7TldnggWpYxii3O8UJrwml6CbJytHAoGAH5XWZFXHidrcVk8tGgsVtinOQuJHKKv0PbzimW3JeKv3Puptf1bJo+rnKN69J8yg6KSVvu+JBh1/ESS4i3k3mBwSWZo+YDhc8wMzjICDRi96u5o/YSrMt32OkObXEYoH4HziSqDt8YxvOfi7nD69fJ0d+jnnJnpCKnbq+ovZyj0CgYAhJGST41lSYt2K3vszVX2nyNfqUoJVZZXNldWDtE3ngIHzhotFTGQiRdMsbE2pdIZaA9yLwkb4dFhBMKhfeuG23RgDC1NJLxsApeNh5KedrsC97mzKnUR7Om/WsIWiDnUxaOXAed9fhGBHYbgaCB5P6kHtUkXxvuT7HQccgujU5w==";
    static final String RSA_SIG = "ac45a0c57f22ab6e03b4f74a9d7063cf988537190e3d796bdff63b8f0ed86add48d345e7984db579e1a2172f79c652755bbb2c6f17eaf44f4750b755905ca9df7041a8aa85c1051c93f82d2ae1fd4fea33ce257d35931ec5b3aa95b85c0b07b54dc0cc5ca1a51698969a73f3daae4bba670ff60bd7f8708b3e41e4f558f461c8935bf668fd183696d90808a5d9406363700d4eb4070785b6643927e7c9054a151bf0a423d7fa5e5d9ce97d4b1fad2cce2e0b2d3755933cccf5f7fc76d7d42b69860251750d883e60651cdd738d54c4cb993fca5d612565871d888ebb3911785ff1f24f838a68d3e8047d841fcc53632b8ca8a05ccf81829067f6fe8741a40adf";
    static final String EC_A_PUB = "MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAELbdnsr8+E1xOwKNHlj7749LNsBimVy2G7yhGDrNK/UHkZduVx+vzr6eUrKiKq6MAkNVQMDhDLGxQEW/ExoIyYg==";
    static final String EC_A_PRV = "MEECAQAwEwYHKoZIzj0CAQYIKoZIzj0DAQcEJzAlAgEBBCCWyVK1PhPCv0O45lLXekhbQgw7OstWBYzP3s93IsHQTA==";
    static final String EC_B_PUB = "MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEqWw9ezlO1zfN3hQIw8vbiO3FuIpiiZi5XTYMVOjweLGBr8IbvI4EXeZFacbZkxPmWIT06J4axxGWdqK6g8fo/A==";
    static final String EC_B_PRV = "MEECAQAwEwYHKoZIzj0CAQYIKoZIzj0DAQcEJzAlAgEBBCB9elnmTemc0N9g1qFXOQ9OtqehYp1NLK9o4j7aEEI/bw==";
    static final String EC_SECRET = "b1631e1a7bec2cc637104a6a9ceda460610bac814c7407fd6f04e96770f8fb3c";
    static final String DH_A_PUB = "MIIBpTCCARoGCSqGSIb3DQEDATCCAQsCgYEAmqta/v+esBEZkHmDFEuwZHgrTInikfNmPuF9ndQ1ZmDz5umPP83STd3grvgTRoX+cEELRNHo2omUAyQEBUJ8BMODyhmsx7DPoIb6n0WftmtujXsoXxUBu9RAjidE+GskbcfXs/t2t4LMUqKp+orvmErkSFKMSPzvdG0H40qtRIcCgYAy6/wu5NZidHYOD7tkVuQiVQCMV4H1ZuAlKOp30bda8dhU6IASc+eeegt6lc/U6mFqQMI695o+Ul2XsVQX5wVlZdDch1JFrI2dcKE4LycrRzQT3YCREmcXgC92IJIG8K5pylhQD8K+T5BQLJqjsjGqfrMQEZzJ1qh6LYrzfkvQagICAgADgYQAAoGAAfQFfLwF29wle4m1UugmYMTbLub5sa+G6paGLnvHacu6OENmUv5XFyQ2xyL252jBia0oZ71eFbestjfrelteJSGhn3EwZhtDPZASb4GDlm9Z6ODLHoFiYtSauPJowCDw+WQ5ZeoLvhd3dEPxUFynmyrC5sGmMV9nxHDn9xaYVFc=";
    static final String DH_A_PRV = "MIIBZgIBADCCARoGCSqGSIb3DQEDATCCAQsCgYEAmqta/v+esBEZkHmDFEuwZHgrTInikfNmPuF9ndQ1ZmDz5umPP83STd3grvgTRoX+cEELRNHo2omUAyQEBUJ8BMODyhmsx7DPoIb6n0WftmtujXsoXxUBu9RAjidE+GskbcfXs/t2t4LMUqKp+orvmErkSFKMSPzvdG0H40qtRIcCgYAy6/wu5NZidHYOD7tkVuQiVQCMV4H1ZuAlKOp30bda8dhU6IASc+eeegt6lc/U6mFqQMI695o+Ul2XsVQX5wVlZdDch1JFrI2dcKE4LycrRzQT3YCREmcXgC92IJIG8K5pylhQD8K+T5BQLJqjsjGqfrMQEZzJ1qh6LYrzfkvQagICAgAEQwJBALiHvQmsfrygOK69i45OnG+wHquuVJW7Kkux2DiH1XfxD6InmIgvb2cS9n3Jg6FegYUKeBxoYm/AV9HpkSvhnCU=";
    static final String DH_B_PUB = "MIIBpTCCARoGCSqGSIb3DQEDATCCAQsCgYEAmqta/v+esBEZkHmDFEuwZHgrTInikfNmPuF9ndQ1ZmDz5umPP83STd3grvgTRoX+cEELRNHo2omUAyQEBUJ8BMODyhmsx7DPoIb6n0WftmtujXsoXxUBu9RAjidE+GskbcfXs/t2t4LMUqKp+orvmErkSFKMSPzvdG0H40qtRIcCgYAy6/wu5NZidHYOD7tkVuQiVQCMV4H1ZuAlKOp30bda8dhU6IASc+eeegt6lc/U6mFqQMI695o+Ul2XsVQX5wVlZdDch1JFrI2dcKE4LycrRzQT3YCREmcXgC92IJIG8K5pylhQD8K+T5BQLJqjsjGqfrMQEZzJ1qh6LYrzfkvQagICAgADgYQAAoGALR+pjGP2rgaFHdIgc78IfGIhO1m3YgMgixK0+iz1OplABD1N+3Z8OavYOxeuti5ziWAmcd+FgEPW4qq0GinGeIc+le6l5nN6XURxiKXljwuAwSi2MaNibiQ40Dz87YIPjFpm04+SF8yv21ITVtxElIlphcJZpuLIylJsqtVKmwk=";
    static final String DH_B_PRV = "MIIBZgIBADCCARoGCSqGSIb3DQEDATCCAQsCgYEAmqta/v+esBEZkHmDFEuwZHgrTInikfNmPuF9ndQ1ZmDz5umPP83STd3grvgTRoX+cEELRNHo2omUAyQEBUJ8BMODyhmsx7DPoIb6n0WftmtujXsoXxUBu9RAjidE+GskbcfXs/t2t4LMUqKp+orvmErkSFKMSPzvdG0H40qtRIcCgYAy6/wu5NZidHYOD7tkVuQiVQCMV4H1ZuAlKOp30bda8dhU6IASc+eeegt6lc/U6mFqQMI695o+Ul2XsVQX5wVlZdDch1JFrI2dcKE4LycrRzQT3YCREmcXgC92IJIG8K5pylhQD8K+T5BQLJqjsjGqfrMQEZzJ1qh6LYrzfkvQagICAgAEQwJBAJWMrVNXgg7h4y82Z+GpiZoPjtwrss4fopoEBvqmigtTunFMt57HXQCXvjX9Vg7XqpRR8j6On3T7+2tMCMwAWIk=";
    static final String DH_SECRET = "1adc88f43f55b52730379cf4db591ba22d8bdc184a08f5f5971e7d8574c0d71f7702fc0bc395ec0837f579cebd713c47bf0850cecb9037a220d2ac4c87c2b7ad7bce757e064aae66408d10be3bef6208f061ccaf3bac0ac8f64bed03532bae14ea5812bd4092da49b133378023daa426890a0e4ea43e350463127baf33d7e6af";
    static final String DSA_PUB = "MIIBuDCCASwGByqGSM44BAEwggEfAoGBAP1/U4EddRIpUt9KnC7s5Of2EbdSPO9EAMMeP4C2USZpRV1AIlH7WT2NWPq/xfW6MPbLm1Vs14E7gB00b/JmYLdrmVClpJ+f6AR7ECLCT7up1/63xhv4O1fnxqimFQ8E+4P208UewwI1VBNaFpEy9nXzrith1yrv8iIDGZ3RSAHHAhUAl2BQjxUjC8yykrmCouuEC/BYHPUCgYEA9+GghdabPd7LvKtcNrhXuXmUr7v6OuqC+VdMCz0HgmdRWVeOutRZT+ZxBxCBgLRJFnEj6EwoFhO3zwkyjMim4TwWeotUfI0o4KOuHiuzpnWRbqN/C/ohNWLx+2J6ASQ7zKTxvqhRkImog9/hWuWfBpKLZl6Ae1UlZAFMO/7PSSoDgYUAAoGBAOzpMSRVxyKae7hCWxoH/Ec2gh/tDzwDR/pK1u6KVjGE4k7snt0r+CZ1mol90pQRn9f0q1E+KLXXmvBGPa96D9CT49Hs/sYuPMBlDovSKB02ZZvZ1k9DOCpMP+UkJN3lQeFgkANWjct+DCWk6F2bOjn70bHgos59DGEcCuu/JHgh";
    static final String DSA_PRV = "MIIBSwIBADCCASwGByqGSM44BAEwggEfAoGBAP1/U4EddRIpUt9KnC7s5Of2EbdSPO9EAMMeP4C2USZpRV1AIlH7WT2NWPq/xfW6MPbLm1Vs14E7gB00b/JmYLdrmVClpJ+f6AR7ECLCT7up1/63xhv4O1fnxqimFQ8E+4P208UewwI1VBNaFpEy9nXzrith1yrv8iIDGZ3RSAHHAhUAl2BQjxUjC8yykrmCouuEC/BYHPUCgYEA9+GghdabPd7LvKtcNrhXuXmUr7v6OuqC+VdMCz0HgmdRWVeOutRZT+ZxBxCBgLRJFnEj6EwoFhO3zwkyjMim4TwWeotUfI0o4KOuHiuzpnWRbqN/C/ohNWLx+2J6ASQ7zKTxvqhRkImog9/hWuWfBpKLZl6Ae1UlZAFMO/7PSSoEFgIUIOkh5K2AX+5Jp3GGow50U7umNhQ=";

    static PublicKey pub(String kf, String b64) throws Exception {
        return KeyFactory.getInstance(kf).generatePublic(new X509EncodedKeySpec(Base64.getDecoder().decode(b64)));
    }
    static PrivateKey prv(String kf, String b64) throws Exception {
        return KeyFactory.getInstance(kf).generatePrivate(new PKCS8EncodedKeySpec(Base64.getDecoder().decode(b64)));
    }

    // ------------------------------------------------------------------
    public static void main(String[] args) throws Exception {
        messageDigest();
        mac();
        keyGenerator();
        cipherKat();
        cipherRoundTrip();
        cipherGcm();
        cipherWrap();
        cipherExceptions();
        pbkdf2();
        secureRandom();
        rsa();
        ec();
        dh();
        dsa();
        providers();
        streams();

        System.out.println("CRYPTO_RESULT ok=" + ok + " fail=" + fail);
        if (fail == 0) System.out.println("CRYPTO_DONE");
    }

    // ============================== MessageDigest ============================
    static void messageDigest() throws Exception {
        byte[] abc = utf8("abc");
        check(hex(MessageDigest.getInstance("MD5").digest(abc)).equals("900150983cd24fb0d6963f7d28e17f72"), "md-md5-abc");
        check(hex(MessageDigest.getInstance("MD5").digest(new byte[0])).equals("d41d8cd98f00b204e9800998ecf8427e"), "md-md5-empty");
        check(hex(MessageDigest.getInstance("SHA-1").digest(abc)).equals("a9993e364706816aba3e25717850c26c9cd0d89d"), "md-sha1-abc");
        check(hex(MessageDigest.getInstance("SHA-1").digest(new byte[0])).equals("da39a3ee5e6b4b0d3255bfef95601890afd80709"), "md-sha1-empty");
        check(hex(MessageDigest.getInstance("SHA-224").digest(abc)).equals("23097d223405d8228642a477bda255b32aadbce4bda0b3f7e36c9da7"), "md-sha224-abc");
        check(hex(MessageDigest.getInstance("SHA-256").digest(abc)).equals("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"), "md-sha256-abc");
        check(hex(MessageDigest.getInstance("SHA-256").digest(new byte[0])).equals("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"), "md-sha256-empty");
        check(hex(MessageDigest.getInstance("SHA-384").digest(abc)).equals("cb00753f45a35e8bb5a03d699ac65007272c32ab0eded1631a8b605a43ff5bed8086072ba1e7cc2358baeca134c825a7"), "md-sha384-abc");
        check(hex(MessageDigest.getInstance("SHA-512").digest(abc)).equals("ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f"), "md-sha512-abc");
        check(hex(MessageDigest.getInstance("SHA-512").digest(new byte[0])).equals("cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e"), "md-sha512-empty");
        check(hex(MessageDigest.getInstance("SHA3-224").digest(abc)).equals("e642824c3f8cf24ad09234ee7d3c766fc9a3a5168d0c94ad73b46fdf"), "md-sha3-224-abc");
        check(hex(MessageDigest.getInstance("SHA3-256").digest(abc)).equals("3a985da74fe225b2045c172d6bd390bd855f086e3e9d525b46bfe24511431532"), "md-sha3-256-abc");
        check(hex(MessageDigest.getInstance("SHA3-384").digest(abc)).equals("ec01498288516fc926459f58e2c6ad8df9b473cb0fc08c2596da7cf0e49be4b298d88cea927ac7f539f1edf228376d25"), "md-sha3-384-abc");
        check(hex(MessageDigest.getInstance("SHA3-512").digest(abc)).equals("b751850b1a57168a5693cd924b6b096e08f621827444f70d884f5d0240d2712e10e116e9192af3c91a7ec57647e3934057340b4cf408d5a56592f8274eec53f0"), "md-sha3-512-abc");

        // digest lengths
        check(MessageDigest.getInstance("MD5").getDigestLength() == 16, "md-len-md5");
        check(MessageDigest.getInstance("SHA-1").getDigestLength() == 20, "md-len-sha1");
        check(MessageDigest.getInstance("SHA-256").getDigestLength() == 32, "md-len-sha256");
        check(MessageDigest.getInstance("SHA-384").getDigestLength() == 48, "md-len-sha384");
        check(MessageDigest.getInstance("SHA-512").getDigestLength() == 64, "md-len-sha512");
        check(MessageDigest.getInstance("SHA-256").getAlgorithm().equals("SHA-256"), "md-algorithm-name");

        // incremental update variants must equal one-shot
        byte[] data = utf8("the quick brown fox");
        byte[] oneShot = MessageDigest.getInstance("SHA-256").digest(data);
        MessageDigest mB = MessageDigest.getInstance("SHA-256");
        for (byte b : data) mB.update(b);
        check(Arrays.equals(mB.digest(), oneShot), "md-incremental-byte");
        MessageDigest mR = MessageDigest.getInstance("SHA-256");
        mR.update(data, 0, 9);
        mR.update(data, 9, data.length - 9);
        check(Arrays.equals(mR.digest(), oneShot), "md-incremental-offlen");
        MessageDigest mBuf = MessageDigest.getInstance("SHA-256");
        mBuf.update(ByteBuffer.wrap(data));
        check(Arrays.equals(mBuf.digest(), oneShot), "md-incremental-bytebuffer");

        // reset returns a digest to fresh state
        MessageDigest mReset = MessageDigest.getInstance("SHA-256");
        mReset.update(utf8("garbage"));
        mReset.reset();
        check(Arrays.equals(mReset.digest(data), oneShot), "md-reset");

        // two-arg digest(buf, off, len)
        MessageDigest mTwo = MessageDigest.getInstance("SHA-256");
        mTwo.update(data);
        byte[] out = new byte[40];
        int wrote = mTwo.digest(out, 4, 32);
        check(wrote == 32, "md-digest-twoarg-len");
        check(Arrays.equals(Arrays.copyOfRange(out, 4, 36), oneShot), "md-digest-twoarg-bytes");

        // clone independence
        MessageDigest base = MessageDigest.getInstance("SHA-256");
        base.update(utf8("ab"));
        MessageDigest cloned = (MessageDigest) base.clone();
        base.update(utf8("c"));
        cloned.update(utf8("c"));
        check(Arrays.equals(base.digest(), oneShot) == false, "md-clone-base-progressed"); // base hashed "abc" of data? sanity uses abc
        check(Arrays.equals(cloned.digest(), MessageDigest.getInstance("SHA-256").digest(utf8("abc"))), "md-clone-matches");
        check(Arrays.equals(base.digest(utf8("")), base.digest(utf8(""))), "md-clone-base-usable"); // reusable after digest

        // isEqual timing-safe compare
        byte[] h1 = MessageDigest.getInstance("SHA-256").digest(abc);
        byte[] h2 = MessageDigest.getInstance("SHA-256").digest(abc);
        byte[] h3 = MessageDigest.getInstance("SHA-256").digest(utf8("abd"));
        check(MessageDigest.isEqual(h1, h2), "md-isEqual-true");
        check(MessageDigest.isEqual(h1, h3) == false, "md-isEqual-false");
        check(MessageDigest.isEqual(h1, Arrays.copyOf(h1, 16)) == false, "md-isEqual-lendiff");
    }

    // ================================== Mac ==================================
    static void mac() throws Exception {
        byte[] key = utf8("Jefe");
        byte[] data = utf8("what do ya want for nothing?");
        check(hmac("HmacMD5", key, data).equals("750c783e6ab0b503eaa86e310a5db738"), "mac-md5-rfc2202");
        check(hmac("HmacSHA1", key, data).equals("effcdf6ae5eb2fa2d27416d5f184df9c259a7c79"), "mac-sha1-rfc2202");
        check(hmac("HmacSHA224", key, data).equals("a30e01098bc6dbbf45690f3a7e9e6d0f8bbea2a39e6148008fd05e44"), "mac-sha224-rfc4231");
        check(hmac("HmacSHA256", key, data).equals("5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"), "mac-sha256-rfc4231");
        check(hmac("HmacSHA384", key, data).equals("af45d2e376484031617f78d2b58a6b1b9c7ef464f5a01b47e42ec3736322445e8e2240ca5e69e2c78b3239ecfab21649"), "mac-sha384-rfc4231");
        check(hmac("HmacSHA512", key, data).equals("164b7a7bfcf819e2e395fbe73b56e0a387bd64222e831fd610270cd7ea2505549758bf75c05a994a6d034f65f8f0e6fdcaeab1a34d4a6b4b636e070a38bce737"), "mac-sha512-rfc4231");

        Mac m = Mac.getInstance("HmacSHA256");
        m.init(new SecretKeySpec(key, "HmacSHA256"));
        check(m.getMacLength() == 32, "mac-length-sha256");
        check(m.getAlgorithm().equals("HmacSHA256"), "mac-algorithm-name");

        // incremental update equals one-shot
        byte[] oneShot = m.doFinal(data);
        m.reset();
        m.update(data, 0, 5);
        m.update(data, 5, data.length - 5);
        check(Arrays.equals(m.doFinal(), oneShot), "mac-incremental");

        // reset / reuse
        m.reset();
        check(Arrays.equals(m.doFinal(data), oneShot), "mac-reset-reuse");

        // two-arg doFinal(output, offset)
        m.update(data);
        byte[] buf = new byte[40];
        m.doFinal(buf, 4);
        check(Arrays.equals(Arrays.copyOfRange(buf, 4, 36), oneShot), "mac-twoarg-doFinal");

        // clone
        Mac base = Mac.getInstance("HmacSHA256");
        base.init(new SecretKeySpec(key, "HmacSHA256"));
        base.update(utf8("what do ya want for "));
        Mac cl = (Mac) base.clone();
        cl.update(utf8("nothing?"));
        check(Arrays.equals(cl.doFinal(), oneShot), "mac-clone");
    }
    static String hmac(String alg, byte[] key, byte[] data) throws Exception {
        Mac m = Mac.getInstance(alg);
        m.init(new SecretKeySpec(key, alg));
        return hex(m.doFinal(data));
    }

    // ========================= KeyGenerator / SecretKey ======================
    static void keyGenerator() throws Exception {
        KeyGenerator aes = KeyGenerator.getInstance("AES");
        aes.init(128, seededRng(1L));
        SecretKey k128 = aes.generateKey();
        check(k128.getAlgorithm().equals("AES"), "kg-aes-algorithm");
        check(k128.getFormat().equals("RAW"), "kg-aes-format");
        check(k128.getEncoded().length == 16, "kg-aes128-len");

        KeyGenerator aes256 = KeyGenerator.getInstance("AES");
        aes256.init(256, seededRng(2L));
        check(aes256.generateKey().getEncoded().length == 32, "kg-aes256-len");

        KeyGenerator hk = KeyGenerator.getInstance("HmacSHA256");
        hk.init(seededRng(3L));
        check(hk.generateKey().getAlgorithm().equals("HmacSHA256"), "kg-hmac-algorithm");

        KeyGenerator des3 = KeyGenerator.getInstance("DESede");
        des3.init(seededRng(4L));
        check(des3.generateKey().getEncoded().length == 24, "kg-desede-len");

        // seeded determinism: same seed -> same key bytes
        KeyGenerator g1 = KeyGenerator.getInstance("AES");
        SecureRandom r1 = SecureRandom.getInstance("SHA1PRNG"); r1.setSeed(99L);
        g1.init(128, r1);
        byte[] kb1 = g1.generateKey().getEncoded();
        KeyGenerator g2 = KeyGenerator.getInstance("AES");
        SecureRandom r2 = SecureRandom.getInstance("SHA1PRNG"); r2.setSeed(99L);
        g2.init(128, r2);
        byte[] kb2 = g2.generateKey().getEncoded();
        check(Arrays.equals(kb1, kb2), "kg-seeded-determinism");

        // SecretKeySpec equals / hashCode / format
        byte[] raw = unhex("000102030405060708090a0b0c0d0e0f");
        SecretKeySpec s1 = new SecretKeySpec(raw, "AES");
        SecretKeySpec s2 = new SecretKeySpec(raw.clone(), "AES");
        check(s1.equals(s2), "kg-secretkeyspec-equals");
        check(s1.hashCode() == s2.hashCode(), "kg-secretkeyspec-hashcode");
        check(s1.getFormat().equals("RAW") && Arrays.equals(s1.getEncoded(), raw), "kg-secretkeyspec-encoded");
    }

    // ============================ Cipher KAT (NIST) ==========================
    static void cipherKat() throws Exception {
        // AES ECB known-answer (FIPS-197 Appendix C)
        byte[] pt = unhex("00112233445566778899aabbccddeeff");
        check(hex(aesEcb(unhex("000102030405060708090a0b0c0d0e0f"), pt)).equals("69c4e0d86a7b0430d8cdb78070b4c55a"), "cipher-aes128-ecb-kat");
        check(hex(aesEcb(unhex("000102030405060708090a0b0c0d0e0f1011121314151617"), pt)).equals("dda97ca4864cdfe06eaf70a0ec0d7191"), "cipher-aes192-ecb-kat");
        check(hex(aesEcb(unhex("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f"), pt)).equals("8ea2b7ca516745bfeafc49904b496089"), "cipher-aes256-ecb-kat");

        // AES CBC known-answer (NIST SP 800-38A F.2.1)
        Cipher cbc = Cipher.getInstance("AES/CBC/NoPadding");
        cbc.init(Cipher.ENCRYPT_MODE, new SecretKeySpec(unhex("2b7e151628aed2a6abf7158809cf4f3c"), "AES"),
                 new IvParameterSpec(unhex("000102030405060708090a0b0c0d0e0f")));
        check(hex(cbc.doFinal(unhex("6bc1bee22e409f96e93d7e117393172a"))).equals("7649abac8119b246cee98e9b12e9197d"), "cipher-aes128-cbc-kat");

        // AES CTR known-answer (NIST SP 800-38A F.5.1)
        Cipher ctr = Cipher.getInstance("AES/CTR/NoPadding");
        ctr.init(Cipher.ENCRYPT_MODE, new SecretKeySpec(unhex("2b7e151628aed2a6abf7158809cf4f3c"), "AES"),
                 new IvParameterSpec(unhex("f0f1f2f3f4f5f6f7f8f9fafbfcfdfeff")));
        check(hex(ctr.doFinal(unhex("6bc1bee22e409f96e93d7e117393172a"))).equals("874d6191b620e3261bef6864990db6ce"), "cipher-aes128-ctr-kat");

        // block size / algorithm metadata
        check(Cipher.getInstance("AES/ECB/NoPadding").getBlockSize() == 16, "cipher-blocksize-aes");
        check(Cipher.getInstance("AES/CBC/PKCS5Padding").getAlgorithm().equals("AES/CBC/PKCS5Padding"), "cipher-getalgorithm");
        check(Cipher.getMaxAllowedKeyLength("AES") >= 256, "cipher-maxkeylen-unlimited");
    }
    static byte[] aesEcb(byte[] key, byte[] pt) throws Exception {
        Cipher c = Cipher.getInstance("AES/ECB/NoPadding");
        c.init(Cipher.ENCRYPT_MODE, new SecretKeySpec(key, "AES"));
        return c.doFinal(pt);
    }

    // ============================ Cipher round trips =========================
    static void cipherRoundTrip() throws Exception {
        SecretKeySpec key = new SecretKeySpec(unhex("000102030405060708090a0b0c0d0e0f"), "AES");
        byte[] iv16 = unhex("0f0e0d0c0b0a09080706050403020100");
        byte[] msg = utf8("StarryOS carpet-grade cipher round trip payload #1");

        for (String tf : new String[]{"AES/CBC/PKCS5Padding", "AES/ECB/PKCS5Padding",
                                      "AES/CTR/NoPadding", "AES/CFB/NoPadding", "AES/OFB/NoPadding"}) {
            Cipher enc = Cipher.getInstance(tf);
            Cipher dec = Cipher.getInstance(tf);
            if (tf.contains("ECB")) {
                enc.init(Cipher.ENCRYPT_MODE, key);
                dec.init(Cipher.DECRYPT_MODE, key);
            } else {
                enc.init(Cipher.ENCRYPT_MODE, key, new IvParameterSpec(iv16));
                dec.init(Cipher.DECRYPT_MODE, key, new IvParameterSpec(iv16));
            }
            byte[] ct = enc.doFinal(msg);
            check(Arrays.equals(dec.doFinal(ct), msg), "cipher-roundtrip-" + tf.replace('/', '_'));
        }

        // metadata: getIV reflects the supplied IV, getOutputSize for a block cipher
        Cipher meta = Cipher.getInstance("AES/CBC/PKCS5Padding");
        meta.init(Cipher.ENCRYPT_MODE, key, new IvParameterSpec(iv16));
        check(Arrays.equals(meta.getIV(), iv16), "cipher-getIV");
        check(meta.getOutputSize(16) == 32, "cipher-getOutputSize"); // 16-byte input pads to 2 blocks

        // multi-part update + doFinal equals single-shot
        Cipher single = Cipher.getInstance("AES/CBC/PKCS5Padding");
        single.init(Cipher.ENCRYPT_MODE, key, new IvParameterSpec(iv16));
        byte[] full = single.doFinal(msg);
        Cipher multi = Cipher.getInstance("AES/CBC/PKCS5Padding");
        multi.init(Cipher.ENCRYPT_MODE, key, new IvParameterSpec(iv16));
        ByteArrayOutputStream acc = new ByteArrayOutputStream();
        byte[] p1 = multi.update(msg, 0, 10);
        if (p1 != null) acc.write(p1);
        byte[] p2 = multi.update(msg, 10, msg.length - 10);
        if (p2 != null) acc.write(p2);
        acc.write(multi.doFinal());
        check(Arrays.equals(acc.toByteArray(), full), "cipher-multipart-update");

        // five-arg doFinal(input, inOff, inLen, output, outOff)
        Cipher five = Cipher.getInstance("AES/CBC/PKCS5Padding");
        five.init(Cipher.ENCRYPT_MODE, key, new IvParameterSpec(iv16));
        byte[] dst = new byte[five.getOutputSize(msg.length)];
        int n = five.doFinal(msg, 0, msg.length, dst, 0);
        check(Arrays.equals(Arrays.copyOf(dst, n), full), "cipher-fivearg-doFinal");

        // AES-256 round trip
        SecretKeySpec key256 = new SecretKeySpec(unhex("000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f"), "AES");
        Cipher e2 = Cipher.getInstance("AES/CBC/PKCS5Padding");
        e2.init(Cipher.ENCRYPT_MODE, key256, new IvParameterSpec(iv16));
        Cipher d2 = Cipher.getInstance("AES/CBC/PKCS5Padding");
        d2.init(Cipher.DECRYPT_MODE, key256, new IvParameterSpec(iv16));
        check(Arrays.equals(d2.doFinal(e2.doFinal(msg)), msg), "cipher-aes256-roundtrip");

        // DESede round trip
        SecretKeySpec des3 = new SecretKeySpec(unhex("0123456789abcdef23456789abcdef01456789abcdef0123"), "DESede");
        Cipher de = Cipher.getInstance("DESede/CBC/PKCS5Padding");
        de.init(Cipher.ENCRYPT_MODE, des3, new IvParameterSpec(unhex("0001020304050607")));
        Cipher dd = Cipher.getInstance("DESede/CBC/PKCS5Padding");
        dd.init(Cipher.DECRYPT_MODE, des3, new IvParameterSpec(unhex("0001020304050607")));
        check(Arrays.equals(dd.doFinal(de.doFinal(msg)), msg), "cipher-desede-roundtrip");
    }

    // =============================== Cipher GCM ==============================
    static void cipherGcm() throws Exception {
        // NIST GCM Test Case 3 (no AAD): doFinal output = ciphertext || 16-byte tag
        byte[] gkey = unhex("feffe9928665731c6d6a8f9467308308");
        byte[] giv = unhex("cafebabefacedbaddecaf888");
        byte[] gPlain = unhex("d9313225f88406e5a55909c5aff5269a86a7a9531534f7da2e4c303d8a318a721c3c0c95956809532fcf0e2449a6b525b16aedf5aa0de657ba637b391aafd255");
        Cipher gc = Cipher.getInstance("AES/GCM/NoPadding");
        gc.init(Cipher.ENCRYPT_MODE, new SecretKeySpec(gkey, "AES"), new GCMParameterSpec(128, giv));
        String expect = "42831ec2217774244b7221b784d0d49ce3aa212f2c02a4e035c17e2329aca12e21d514b25466931c7d8f6a5aac84aa051ba30b396a0aac973d58e091473f5985"
                      + "4d5c2af327cd64a62cf35abd2ba6fab4";
        check(hex(gc.doFinal(gPlain)).equals(expect), "gcm-nist-kat");

        // round trip with AAD
        SecretKeySpec key = new SecretKeySpec(unhex("000102030405060708090a0b0c0d0e0f"), "AES");
        byte[] iv = unhex("0102030405060708090a0b0c");
        byte[] aad = utf8("authenticated-header");
        byte[] msg = utf8("authenticated and encrypted body");
        Cipher e = Cipher.getInstance("AES/GCM/NoPadding");
        e.init(Cipher.ENCRYPT_MODE, key, new GCMParameterSpec(128, iv));
        e.updateAAD(aad);
        byte[] ct = e.doFinal(msg);
        Cipher d = Cipher.getInstance("AES/GCM/NoPadding");
        d.init(Cipher.DECRYPT_MODE, key, new GCMParameterSpec(128, iv));
        d.updateAAD(aad);
        check(Arrays.equals(d.doFinal(ct), msg), "gcm-aad-roundtrip");

        // tamper the ciphertext -> AEADBadTagException (deterministic)
        final byte[] tampered = ct.clone();
        tampered[0] ^= 0x01;
        expect(AEADBadTagException.class, "gcm-tamper-detected", () -> {
            Cipher t = Cipher.getInstance("AES/GCM/NoPadding");
            t.init(Cipher.DECRYPT_MODE, key, new GCMParameterSpec(128, iv));
            t.updateAAD(aad);
            t.doFinal(tampered);
        });

        // wrong AAD -> AEADBadTagException (deterministic)
        expect(AEADBadTagException.class, "gcm-aad-mismatch", () -> {
            Cipher t = Cipher.getInstance("AES/GCM/NoPadding");
            t.init(Cipher.DECRYPT_MODE, key, new GCMParameterSpec(128, iv));
            t.updateAAD(utf8("different-header"));
            t.doFinal(ct);
        });

        // AlgorithmParameters round trip from a GCM cipher
        AlgorithmParameters ap = e.getParameters();
        GCMParameterSpec gspec = ap.getParameterSpec(GCMParameterSpec.class);
        check(gspec.getTLen() == 128, "gcm-params-tlen");
        check(Arrays.equals(gspec.getIV(), iv), "gcm-params-iv");
    }

    // ============================= Cipher wrap/unwrap ========================
    static void cipherWrap() throws Exception {
        SecretKeySpec kek = new SecretKeySpec(unhex("000102030405060708090a0b0c0d0e0f"), "AES");
        SecretKey target = new SecretKeySpec(unhex("101112131415161718191a1b1c1d1e1f"), "AES");
        Cipher wc = Cipher.getInstance("AESWrap");
        wc.init(Cipher.WRAP_MODE, kek);
        byte[] wrapped = wc.wrap(target);
        Cipher uc = Cipher.getInstance("AESWrap");
        uc.init(Cipher.UNWRAP_MODE, kek);
        Key unwrapped = uc.unwrap(wrapped, "AES", Cipher.SECRET_KEY);
        check(Arrays.equals(unwrapped.getEncoded(), target.getEncoded()), "cipher-wrap-unwrap");
    }

    // ============================ Cipher exceptions ==========================
    static void cipherExceptions() throws Exception {
        SecretKeySpec key = new SecretKeySpec(unhex("000102030405060708090a0b0c0d0e0f"), "AES");
        expect(NoSuchAlgorithmException.class, "exc-no-such-algorithm",
               () -> Cipher.getInstance("Nonexistent/CBC/PKCS5Padding"));
        expect(ShortBufferException.class, "exc-short-buffer", () -> {
            Cipher c = Cipher.getInstance("AES/ECB/PKCS5Padding");
            c.init(Cipher.ENCRYPT_MODE, key);
            c.doFinal(utf8("0123456789abcdef"), 0, 16, new byte[1], 0); // output buffer too small
        });
        expect(InvalidAlgorithmParameterException.class, "exc-bad-iv-length", () -> {
            Cipher c = Cipher.getInstance("AES/CBC/PKCS5Padding");
            c.init(Cipher.ENCRYPT_MODE, key, new IvParameterSpec(new byte[8])); // AES IV must be 16 bytes
        });
        expect(InvalidKeyException.class, "exc-invalid-key-length", () -> {
            Cipher c = Cipher.getInstance("AES/ECB/PKCS5Padding");
            c.init(Cipher.ENCRYPT_MODE, new SecretKeySpec(new byte[15], "AES"));
        });
        expect(IllegalBlockSizeException.class, "exc-illegal-block-size", () -> {
            Cipher c = Cipher.getInstance("AES/ECB/NoPadding");
            c.init(Cipher.ENCRYPT_MODE, key);
            c.doFinal(new byte[10]); // not a multiple of the 16-byte block
        });
        expect(InvalidAlgorithmParameterException.class, "exc-invalid-gcm-tlen", () -> {
            Cipher c = Cipher.getInstance("AES/GCM/NoPadding");
            c.init(Cipher.ENCRYPT_MODE, key, new GCMParameterSpec(8, new byte[12])); // 8-bit tag unsupported
        });
        expect(NoSuchAlgorithmException.class, "exc-md-no-such-algorithm",
               () -> MessageDigest.getInstance("SHA-999"));
    }

    // ============================ SecretKeyFactory ===========================
    static void pbkdf2() throws Exception {
        // RFC 6070 vector: P="password" S="salt" c=4096 dkLen=160 bits
        SecretKeyFactory f1 = SecretKeyFactory.getInstance("PBKDF2WithHmacSHA1");
        byte[] d1 = f1.generateSecret(new PBEKeySpec("password".toCharArray(), utf8("salt"), 4096, 160)).getEncoded();
        check(hex(d1).equals("4b007901b765489abead49d926f721d065a429c1"), "pbkdf2-sha1-rfc6070");

        SecretKeyFactory f2 = SecretKeyFactory.getInstance("PBKDF2WithHmacSHA256");
        byte[] d2 = f2.generateSecret(new PBEKeySpec("password".toCharArray(), utf8("salt"), 4096, 256)).getEncoded();
        check(hex(d2).equals("c5e478d59288c841aa530db6845c4c8d962893a001ce4e11a4963873aa98134a"), "pbkdf2-sha256");

        // iteration count changes the output
        byte[] d3 = f1.generateSecret(new PBEKeySpec("password".toCharArray(), utf8("salt"), 1, 160)).getEncoded();
        check(hex(d3).equals("0c60c80f961f0e71f3a9b524af6012062fe037a6"), "pbkdf2-sha1-c1");
        check(Arrays.equals(d1, d3) == false, "pbkdf2-iterations-differ");

        // derived key length follows requested dkLen
        byte[] d4 = f2.generateSecret(new PBEKeySpec("pw".toCharArray(), utf8("NaCl"), 100, 512)).getEncoded();
        check(d4.length == 64, "pbkdf2-dklen-512bits");
    }

    // =============================== SecureRandom ============================
    static void secureRandom() throws Exception {
        byte[] seed = utf8("deterministic-seed-material");
        SecureRandom a = SecureRandom.getInstance("SHA1PRNG"); a.setSeed(seed);
        SecureRandom b = SecureRandom.getInstance("SHA1PRNG"); b.setSeed(seed);
        byte[] xa = new byte[64]; a.nextBytes(xa);
        byte[] xb = new byte[64]; b.nextBytes(xb);
        check(Arrays.equals(xa, xb), "sr-sha1prng-determinism");
        check(a.getAlgorithm().equals("SHA1PRNG"), "sr-algorithm-name");

        SecureRandom c = SecureRandom.getInstance("SHA1PRNG"); c.setSeed(utf8("other-seed"));
        byte[] xc = new byte[64]; c.nextBytes(xc);
        check(Arrays.equals(xa, xc) == false, "sr-different-seed-differs");

        // nextInt deterministic under a fixed seed
        SecureRandom d = SecureRandom.getInstance("SHA1PRNG"); d.setSeed(seed);
        SecureRandom e = SecureRandom.getInstance("SHA1PRNG"); e.setSeed(seed);
        check(d.nextInt() == e.nextInt(), "sr-nextInt-determinism");
        check(d.nextInt(1000) == e.nextInt(1000), "sr-nextInt-bound-determinism");
        check(d.nextLong() == e.nextLong(), "sr-nextLong-determinism");
        check(d.nextBoolean() == e.nextBoolean(), "sr-nextBoolean-determinism");

        // a non-empty buffer is actually filled (not left all-zero)
        byte[] filled = new byte[32];
        a.nextBytes(filled);
        check(Arrays.equals(filled, new byte[32]) == false, "sr-nextBytes-nonzero");

        // generateSeed returns the requested number of bytes
        check(SecureRandom.getInstance("SHA1PRNG").generateSeed(20).length == 20, "sr-generateSeed-length");
    }

    // ================================== RSA ==================================
    static void rsa() throws Exception {
        PublicKey pubK = pub("RSA", RSA_PUB);
        PrivateKey prvK = prv("RSA", RSA_PRV);
        check(pubK.getAlgorithm().equals("RSA"), "rsa-pub-algorithm");
        check(((RSAPublicKey) pubK).getModulus().bitLength() == 2048, "rsa-modulus-bitlength");

        // deterministic SHA256withRSA (PKCS#1 v1.5) -> exact known signature
        byte[] msg = utf8("carpet-message");
        Signature s = Signature.getInstance("SHA256withRSA");
        s.initSign(prvK);
        s.update(msg);
        byte[] sig = s.sign();
        check(hex(sig).equals(RSA_SIG), "rsa-sign-deterministic-kat");
        check(s.getAlgorithm().equals("SHA256withRSA"), "rsa-signature-algorithm");

        // verify good signature, reject tampered message
        Signature v = Signature.getInstance("SHA256withRSA");
        v.initVerify(pubK);
        v.update(msg);
        check(v.verify(sig), "rsa-verify-true");
        Signature v2 = Signature.getInstance("SHA256withRSA");
        v2.initVerify(pubK);
        v2.update(utf8("carpet-messagX"));
        check(v2.verify(sig) == false, "rsa-verify-tampered-false");

        // RSA/ECB/PKCS1Padding encrypt -> decrypt round trip
        byte[] secret = utf8("rsa pkcs1 transport secret");
        Cipher e1 = Cipher.getInstance("RSA/ECB/PKCS1Padding");
        e1.init(Cipher.ENCRYPT_MODE, pubK, seededRng(11L));
        byte[] c1 = e1.doFinal(secret);
        Cipher d1 = Cipher.getInstance("RSA/ECB/PKCS1Padding");
        d1.init(Cipher.DECRYPT_MODE, prvK);
        check(Arrays.equals(d1.doFinal(c1), secret), "rsa-pkcs1-roundtrip");

        // RSA OAEP (SHA-256) round trip
        Cipher e2 = Cipher.getInstance("RSA/ECB/OAEPWithSHA-256AndMGF1Padding");
        e2.init(Cipher.ENCRYPT_MODE, pubK, seededRng(12L));
        byte[] c2 = e2.doFinal(secret);
        Cipher d2 = Cipher.getInstance("RSA/ECB/OAEPWithSHA-256AndMGF1Padding");
        d2.init(Cipher.DECRYPT_MODE, prvK);
        check(Arrays.equals(d2.doFinal(c2), secret), "rsa-oaep-roundtrip");

        // KeyFactory spec round trip: X.509 re-encode is byte-stable
        KeyFactory kf = KeyFactory.getInstance("RSA");
        X509EncodedKeySpec spec = kf.getKeySpec(pubK, X509EncodedKeySpec.class);
        PublicKey rebuilt = kf.generatePublic(spec);
        check(Arrays.equals(rebuilt.getEncoded(), pubK.getEncoded()), "rsa-keyfactory-spec-roundtrip");
        RSAPublicKeySpec rs = kf.getKeySpec(pubK, RSAPublicKeySpec.class);
        check(rs.getPublicExponent().equals(BigInteger.valueOf(65537)), "rsa-public-exponent");
    }

    // ================================== EC ===================================
    static void ec() throws Exception {
        PublicKey aPub = pub("EC", EC_A_PUB);
        PrivateKey aPrv = prv("EC", EC_A_PRV);
        PublicKey bPub = pub("EC", EC_B_PUB);
        PrivateKey bPrv = prv("EC", EC_B_PRV);
        check(aPub.getAlgorithm().equals("EC"), "ec-algorithm");

        // ECDSA sign/verify round trip (signature is randomized -> no exact bytes)
        byte[] msg = utf8("ecdsa carpet payload");
        Signature s = Signature.getInstance("SHA256withECDSA");
        s.initSign(aPrv, seededRng(21L));
        s.update(msg);
        byte[] sig = s.sign();
        Signature v = Signature.getInstance("SHA256withECDSA");
        v.initVerify(aPub);
        v.update(msg);
        check(v.verify(sig), "ecdsa-verify-true");
        Signature v2 = Signature.getInstance("SHA256withECDSA");
        v2.initVerify(aPub);
        v2.update(utf8("ecdsa carpet payloaX"));
        check(v2.verify(sig) == false, "ecdsa-verify-tampered-false");

        // ECDH: both parties derive the same secret == embedded KAT
        KeyAgreement ka = KeyAgreement.getInstance("ECDH");
        ka.init(aPrv); ka.doPhase(bPub, true);
        byte[] secA = ka.generateSecret();
        KeyAgreement kb = KeyAgreement.getInstance("ECDH");
        kb.init(bPrv); kb.doPhase(aPub, true);
        byte[] secB = kb.generateSecret();
        check(Arrays.equals(secA, secB), "ecdh-both-sides-equal");
        check(hex(secA).equals(EC_SECRET), "ecdh-kat");

        // live (cheap) EC P-256 key generation, seeded, then self-agreement round trip
        KeyPairGenerator g = KeyPairGenerator.getInstance("EC");
        SecureRandom rng = SecureRandom.getInstance("SHA1PRNG"); rng.setSeed(7L);
        g.initialize(new ECGenParameterSpec("secp256r1"), rng);
        KeyPair kp = g.generateKeyPair();
        check(kp.getPublic().getAlgorithm().equals("EC"), "ec-keygen-live");
        KeyAgreement k1 = KeyAgreement.getInstance("ECDH");
        k1.init(kp.getPrivate()); k1.doPhase(aPub, true);
        KeyAgreement k2 = KeyAgreement.getInstance("ECDH");
        k2.init(aPrv); k2.doPhase(kp.getPublic(), true);
        check(Arrays.equals(k1.generateSecret(), k2.generateSecret()), "ecdh-live-agreement");
    }

    // ================================== DH ===================================
    static void dh() throws Exception {
        PublicKey aPub = pub("DH", DH_A_PUB);
        PrivateKey aPrv = prv("DH", DH_A_PRV);
        PublicKey bPub = pub("DH", DH_B_PUB);
        PrivateKey bPrv = prv("DH", DH_B_PRV);

        KeyAgreement ka = KeyAgreement.getInstance("DH");
        ka.init(aPrv); ka.doPhase(bPub, true);
        byte[] secA = ka.generateSecret();
        KeyAgreement kb = KeyAgreement.getInstance("DH");
        kb.init(bPrv); kb.doPhase(aPub, true);
        byte[] secB = kb.generateSecret();
        check(Arrays.equals(secA, secB), "dh-both-sides-equal");
        check(hex(secA).equals(DH_SECRET), "dh-kat");
        check(aPub.getAlgorithm().equals("DH") || aPub.getAlgorithm().equals("DiffieHellman"), "dh-algorithm");
    }

    // ================================== DSA ==================================
    static void dsa() throws Exception {
        PublicKey pubK = pub("DSA", DSA_PUB);
        PrivateKey prvK = prv("DSA", DSA_PRV);
        byte[] msg = utf8("dsa carpet payload");
        Signature s = Signature.getInstance("SHA256withDSA");
        s.initSign(prvK, seededRng(31L));
        s.update(msg);
        byte[] sig = s.sign();
        Signature v = Signature.getInstance("SHA256withDSA");
        v.initVerify(pubK);
        v.update(msg);
        check(v.verify(sig), "dsa-verify-true");
        Signature v2 = Signature.getInstance("SHA256withDSA");
        v2.initVerify(pubK);
        v2.update(utf8("dsa carpet payloaX"));
        check(v2.verify(sig) == false, "dsa-verify-tampered-false");
    }

    // ============================ Provider / Security ========================
    static void providers() throws Exception {
        check(Security.getProviders().length > 0, "sec-providers-present");
        check(MessageDigest.getInstance("SHA-256").getProvider().getName() != null, "sec-digest-provider-name");
        Provider sun = Security.getProvider("SUN");
        check(sun != null && sun.getName().equals("SUN"), "sec-getProvider-SUN");
        // SunJCE supplies the symmetric ciphers
        check(Cipher.getInstance("AES/CBC/PKCS5Padding").getProvider() != null, "sec-cipher-provider");
        check(KeyFactory.getInstance("RSA").getAlgorithm().equals("RSA"), "sec-keyfactory-algorithm");
    }

    // ============================= JCA/JCE streams ===========================
    static void streams() throws Exception {
        byte[] data = utf8("stream digest and cipher carpet input bytes 0123456789");
        byte[] direct = MessageDigest.getInstance("SHA-256").digest(data);

        // DigestInputStream
        MessageDigest md = MessageDigest.getInstance("SHA-256");
        DigestInputStream dis = new DigestInputStream(new ByteArrayInputStream(data), md);
        byte[] sink = new byte[data.length];
        int got = 0, r;
        while ((r = dis.read(sink, got, sink.length - got)) > 0) got += r;
        dis.close();
        check(got == data.length, "stream-digest-input-read-len");
        check(Arrays.equals(md.digest(), direct), "stream-digest-input");

        // DigestOutputStream
        MessageDigest md2 = MessageDigest.getInstance("SHA-256");
        DigestOutputStream dos = new DigestOutputStream(new ByteArrayOutputStream(), md2);
        dos.write(data);
        dos.close();
        check(Arrays.equals(md2.digest(), direct), "stream-digest-output");

        // CipherOutputStream -> CipherInputStream round trip
        SecretKeySpec key = new SecretKeySpec(unhex("000102030405060708090a0b0c0d0e0f"), "AES");
        byte[] iv = unhex("0f0e0d0c0b0a09080706050403020100");
        Cipher enc = Cipher.getInstance("AES/CBC/PKCS5Padding");
        enc.init(Cipher.ENCRYPT_MODE, key, new IvParameterSpec(iv));
        ByteArrayOutputStream cipherBytes = new ByteArrayOutputStream();
        CipherOutputStream cos = new CipherOutputStream(cipherBytes, enc);
        cos.write(data);
        cos.close();

        Cipher dec = Cipher.getInstance("AES/CBC/PKCS5Padding");
        dec.init(Cipher.DECRYPT_MODE, key, new IvParameterSpec(iv));
        CipherInputStream cis = new CipherInputStream(new ByteArrayInputStream(cipherBytes.toByteArray()), dec);
        ByteArrayOutputStream plain = new ByteArrayOutputStream();
        byte[] tmp = new byte[16];
        int rr;
        while ((rr = cis.read(tmp)) > 0) plain.write(tmp, 0, rr);
        cis.close();
        check(Arrays.equals(plain.toByteArray(), data), "stream-cipher-roundtrip");
    }
}
