pub(crate) const SIMPLE: &[u8] = b"--X-BOUNDARY\r\n\
content-disposition: form-data; name=\"field1\"\r\n\
content-type: text/plain;charset=UTF-8\r\n\
content-transfer-encoding: quoted-printable\r\n\
\r\n\
Joe owes =E2=82=AC100.\r\n\
--X-BOUNDARY--\r\n";

pub(crate) const MULTI: &[u8] = b"----BoundaryjXo5N4HEAXWcKrw7\r\n\
Content-Disposition: form-data; name=\"field1\"\r\n\
\r\n\
value1\r\n\
----BoundaryjXo5N4HEAXWcKrw7\r\n\
Content-Disposition: form-data; name=\"field2\"\r\n\
\r\n\
value2\r\n\
----BoundaryjXo5N4HEAXWcKrw7\r\n\
Content-Disposition: form-data; name=\"file1\"; filename=\"dummy.txt\"\r\n\
Content-Type: foo\r\n\
\r\n\
Hello World!\r\n\
----BoundaryjXo5N4HEAXWcKrw7--\r\n";
