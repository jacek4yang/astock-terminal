using @shared {app_request}

///|
#proton.command(contract=app_request)
async fn Backend::app_request(
  self : Backend,
  request : @shared.AppRequest,
) -> Json raise {
  self.route(request)
}
