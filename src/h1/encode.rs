use crate::{
    buffet::IoChunkList,
    types::{Headers, Request, Response},
};

pub(crate) fn encode_request(req: Request, list: &mut IoChunkList) -> eyre::Result<()> {
    list.push(req.method.into_chunk());
    list.push(" ");
    list.push(req.path);
    match req.version {
        1 => list.push(" HTTP/1.1\r\n"),
        _ => return Err(eyre::eyre!("unsupported HTTP version 1.{}", req.version)),
    }
    for header in req.headers {
        list.push(header.name);
        list.push(": ");
        list.push(header.value);
        list.push("\r\n");
    }
    list.push("\r\n");
    Ok(())
}

pub(crate) fn encode_response(res: Response, list: &mut IoChunkList) -> eyre::Result<()> {
    match res.version {
        1 => list.push(&b"HTTP/1.1 "[..]),
        _ => return Err(eyre::eyre!("unsupported HTTP version 1.{}", res.version)),
    }
    // FIXME: wasteful
    let code = res.code.to_string();
    list.push(code.into_bytes());
    list.push(" ");
    list.push(res.reason);
    list.push("\r\n");
    encode_headers(res.headers, list)?;
    list.push("\r\n");
    Ok(())
}

pub(crate) fn encode_headers(headers: Headers, list: &mut IoChunkList) -> eyre::Result<()> {
    for header in headers {
        list.push(header.name);
        list.push(": ");
        list.push(header.value);
        list.push("\r\n");
    }
    Ok(())
}
