use crate::{
    buffet::PieceList,
    types::{Headers, Request, Response},
};

pub(crate) fn encode_request(req: Request, list: &mut PieceList) -> eyre::Result<()> {
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

pub(crate) fn encode_response(res: Response, list: &mut PieceList) -> eyre::Result<()> {
    match res.version {
        1 => list.push(&b"HTTP/1.1 "[..]),
        _ => return Err(eyre::eyre!("unsupported HTTP version 1.{}", res.version)),
    }

    // cf. https://github.com/hyperium/http/pull/569 - it's already 'static,
    // the function signature just doesn't reflect it
    let status_str: &'static str = unsafe { std::mem::transmute(res.status.as_str()) };

    list.push(status_str);
    list.push(" ");
    list.push(res.status.canonical_reason().unwrap_or("Unknown"));
    list.push("\r\n");
    encode_headers(res.headers, list)?;
    list.push("\r\n");
    Ok(())
}

pub(crate) fn encode_headers(headers: Headers, list: &mut PieceList) -> eyre::Result<()> {
    for header in headers {
        list.push(header.name);
        list.push(": ");
        list.push(header.value);
        list.push("\r\n");
    }
    Ok(())
}
