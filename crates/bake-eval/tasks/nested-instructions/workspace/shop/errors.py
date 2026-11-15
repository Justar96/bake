"""Error responses of the router and of services that have no helpers of their own."""


def not_found(message):
    return 404, {"message": message}


def forbidden(message):
    return 403, {"message": message}


def bad_request(message):
    return 400, {"message": message}
