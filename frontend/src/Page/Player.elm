module Page.Player exposing (Model, Msg(..), init, mediaErrorMessage, update, view)

import Api exposing (Entry(..))
import Html exposing (..)
import Html.Attributes exposing (..)
import Html.Events exposing (on)
import Http
import Json.Decode as D
import Route


type Model
    = Loading String
    | Loaded PlayerState
    | Failed String


type alias PlayerState =
    { path : String
    , title : String
    , uploadDate : Maybe String
    , durationSecs : Maybe Float
    , compatPath : Maybe String
    , nativeType : Maybe String
    , description : Maybe String
    , channel : Maybe String
    , channelUrl : Maybe String
    , webpageUrl : Maybe String
    , viewCount : Maybe Int
    , bufferFraction : Float
    , mediaError : Maybe MediaErrorCode
    }


{-| MediaError.code values from the HTML media element spec.
-}
type MediaErrorCode
    = ErrAborted
    | ErrNetwork
    | ErrDecode
    | ErrSrcNotSupported
    | ErrUnknown Int


type Msg
    = GotListing (Result Http.Error Api.DirListing)
    | VideoCanPlay
    | VideoProgress Float
    | MediaError MediaErrorCode


init : String -> ( Model, Cmd Msg )
init path =
    ( Loading path
    , Api.getBrowse (parentPath path) GotListing
    )


update : Msg -> Model -> ( Model, Cmd Msg )
update msg model =
    case msg of
        GotListing (Ok listing) ->
            ( Loaded (findVideo (loadingPath model) listing.entries)
            , Cmd.none
            )

        GotListing (Err _) ->
            ( Loaded (fallbackState (loadingPath model))
            , Cmd.none
            )

        VideoCanPlay ->
            case model of
                Loaded state ->
                    ( Loaded { state | bufferFraction = 1 }, Cmd.none )

                _ ->
                    ( model, Cmd.none )

        VideoProgress fraction ->
            case model of
                Loaded state ->
                    ( Loaded { state | bufferFraction = fraction }, Cmd.none )

                _ ->
                    ( model, Cmd.none )

        MediaError code ->
            case model of
                Loaded state ->
                    ( Loaded { state | mediaError = Just code }, Cmd.none )

                _ ->
                    ( model, Cmd.none )


{-| Search the directory listing for the video matching the given path
and convert it to a PlayerState. Falls back to a minimal state when
the video is not found in the listing.
-}
findVideo : String -> List Api.Entry -> PlayerState
findVideo path entries =
    entries
        |> List.filterMap (matchVideo path)
        |> List.head
        |> Maybe.withDefault (fallbackState path)


matchVideo : String -> Api.Entry -> Maybe PlayerState
matchVideo path entry =
    case entry of
        Video v ->
            if v.path == path then
                Just
                    { path = v.path
                    , title = Maybe.withDefault v.name v.title
                    , uploadDate = v.uploadDate
                    , durationSecs = v.durationSecs
                    , compatPath = v.compatPath
                    , nativeType = v.nativeType
                    , description = v.description
                    , channel = v.channel
                    , channelUrl = v.channelUrl
                    , webpageUrl = v.webpageUrl
                    , viewCount = v.viewCount
                    , bufferFraction = 0
                    , mediaError = Nothing
                    }

            else
                Nothing

        _ ->
            Nothing


fallbackState : String -> PlayerState
fallbackState path =
    { path = path
    , title = path
    , uploadDate = Nothing
    , durationSecs = Nothing
    , compatPath = Nothing
    , nativeType = Nothing
    , description = Nothing
    , channel = Nothing
    , channelUrl = Nothing
    , webpageUrl = Nothing
    , viewCount = Nothing
    , bufferFraction = 0
    , mediaError = Nothing
    }



-- VIEW


view : Model -> Html Msg
view model =
    case model of
        Loading _ ->
            p [ style "padding" "1rem" ] [ text "Loading…" ]

        Failed err ->
            p [ style "padding" "1rem", style "color" "var(--color-error)" ]
                [ text ("Error: " ++ err) ]

        Loaded state ->
            viewLoaded state


viewLoaded : PlayerState -> Html Msg
viewLoaded state =
    div [ style "padding" "1rem" ]
        ([ backLink state.path
         , case state.mediaError of
            Just code ->
                viewMediaError state.path code

            Nothing ->
                viewPlayer state
         , h2 [ style "margin-top" "0.75rem" ] [ text state.title ]
         ]
            ++ viewMetadata state
        )


backLink : String -> Html Msg
backLink path =
    a
        [ href (Route.toString (Route.Browse { path = parentPath path, query = "", page = 1 }))
        , style "margin-bottom" "1rem"
        , style "display" "inline-block"
        ]
        [ text "← Back" ]


viewMediaError : String -> MediaErrorCode -> Html Msg
viewMediaError path code =
    div
        [ style "background" "var(--color-surface)"
        , style "padding" "2rem"
        , style "max-width" "960px"
        , style "text-align" "center"
        ]
        [ p [ style "color" "var(--color-error)" ]
            [ text (mediaErrorMessage code) ]
        , a
            [ href (Api.videoUrl path)
            , attribute "download" ""
            ]
            [ text "Download to play in VLC or another media player" ]
        ]


viewPlayer : PlayerState -> Html Msg
viewPlayer state =
    let
        ( extraAttrs, sources ) =
            videoSources state
    in
    div []
        [ bufferBar state.bufferFraction
        , video (videoAttrs ++ extraAttrs) sources
        ]


videoAttrs : List (Attribute Msg)
videoAttrs =
    [ controls True
    , on "canplay" (D.succeed VideoCanPlay)
    , style "width" "100%"
    , style "max-width" "960px"
    , style "display" "block"
    ]


{-| Build the extra attributes and child source elements for the video
tag from the two facts the server derives: `nativeType` (the codecs-qualified
`<source type>` for the native file, or Nothing when it must not be offered)
and `compatPath` (a universally-playable H.264/AAC MP4, when one exists).

The native source, when offered, carries a full RFC 6381 codecs string so the
browser's canPlayType decides authoritatively — an incapable browser (e.g.
Safari facing AV1/VP9) skips it without downloading a byte and drops to the
compat, while a capable browser plays the higher-quality native. The compat is
the terminal fallback with a bare `video/mp4` type, which every browser
accepts.

-}
videoSources :
    PlayerState
    -> ( List (Attribute Msg), List (Html Msg) )
videoSources state =
    let
        nativeUrl =
            Api.videoUrl state.path

        -- Source elements do not expose target.error.code, so a source-level
        -- failure reports the fixed "not supported" code.
        fixedError =
            on "error" (D.succeed (MediaError ErrSrcNotSupported))

        -- The video element does expose target.error.code, so a src-level
        -- failure reports the specific decode/network/format cause.
        codedError =
            on "error" (D.map MediaError mediaErrorDecoder)
    in
    case ( state.nativeType, state.compatPath ) of
        ( Just nt, Just cp ) ->
            ( []
            , [ source [ src nativeUrl, type_ nt ] []
              , source [ src (Api.videoUrl cp), type_ "video/mp4", fixedError ] []
              ]
            )

        ( Just nt, Nothing ) ->
            -- Only the native, but its codecs type still lets an incapable
            -- browser fail fast instead of downloading it.
            ( []
            , [ source [ src nativeUrl, type_ nt, fixedError ] [] ]
            )

        ( Nothing, Just cp ) ->
            -- Native cannot be offered safely (non-standard container or
            -- unknown codecs); serve only the guaranteed-playable compat.
            ( [ src (Api.videoUrl cp), codedError ], [] )

        ( Nothing, Nothing ) ->
            -- Best effort with no hint: let the browser probe the native and
            -- surface the specific error if it cannot play it.
            ( [ src nativeUrl, codedError ], [] )


bufferBar : Float -> Html msg
bufferBar fraction =
    if fraction < 1 then
        div
            [ style "width" "100%"
            , style "max-width" "960px"
            , style "height" "4px"
            , style "background" "var(--color-surface)"
            , style "margin-bottom" "0.25rem"
            ]
            [ div
                [ style "height" "100%"
                , style "width" (String.fromFloat (fraction * 100) ++ "%")
                , style "background" "var(--color-link)"
                , style "transition" "width 0.3s ease"
                ]
                []
            ]

    else
        text ""


viewMetadata : PlayerState -> List (Html Msg)
viewMetadata state =
    List.filterMap identity
        [ state.uploadDate
            |> Maybe.map (\d -> p [] [ text ("Uploaded: " ++ formatDate d) ])
        , state.durationSecs
            |> Maybe.map (\s -> p [] [ text ("Duration: " ++ formatDuration s) ])
        , state.viewCount
            |> Maybe.map (\n -> p [] [ text ("Views: " ++ formatViewCount n) ])
        , state.channel
            |> Maybe.map (viewChannel state.channelUrl)
        , state.webpageUrl
            |> Maybe.map viewYoutubeLink
        , state.description
            |> Maybe.map viewDescription
        ]


viewChannel : Maybe String -> String -> Html msg
viewChannel channelUrl ch =
    p []
        [ text "Channel: "
        , case channelUrl of
            Just url ->
                a [ href url, target "_blank", attribute "rel" "noopener noreferrer" ]
                    [ text ch ]

            Nothing ->
                text ch
        ]


viewYoutubeLink : String -> Html msg
viewYoutubeLink url =
    p []
        [ a [ href url, target "_blank", attribute "rel" "noopener noreferrer" ]
            [ text "Watch on YouTube" ]
        ]


viewDescription : String -> Html msg
viewDescription desc =
    div
        [ style "margin-top" "1rem"
        , style "max-width" "960px"
        , style "max-height" "14rem"
        , style "overflow-y" "auto"
        , style "font-size" "0.9rem"
        , style "line-height" "1.6"
        , style "white-space" "pre-wrap"
        , style "word-break" "break-word"
        ]
        (renderDescription desc)



-- HELPERS


parentPath : String -> String
parentPath path =
    path
        |> String.split "/"
        |> List.reverse
        |> List.drop 1
        |> List.reverse
        |> String.join "/"


loadingPath : Model -> String
loadingPath model =
    case model of
        Loading p ->
            p

        _ ->
            ""


{-| Strip trailing punctuation characters that are unlikely to be part of a
URL even though they are technically valid. If they were intentional they
would normally be percent-encoded.
-}
stripTrailingPunct : String -> ( String, String )
stripTrailingPunct s =
    let
        punctChars =
            [ '.', ',', ';', ')', ']', '!', '"', '\'', '>', '?' ]

        dropRight str =
            case String.uncons (String.reverse str) of
                Just ( c, rest ) ->
                    if List.member c punctChars then
                        dropRight (String.reverse rest)

                    else
                        str

                Nothing ->
                    str

        url =
            dropRight s
    in
    ( url, String.dropLeft (String.length url) s )


{-| Render a description string, turning http/https tokens into clickable
links. Newlines are preserved by the parent's white-space: pre-wrap style.
-}
renderDescription : String -> List (Html msg)
renderDescription desc =
    let
        isUrl w =
            String.startsWith "http://" w || String.startsWith "https://" w

        renderWord w =
            if isUrl w then
                let
                    ( url, trailing ) =
                        stripTrailingPunct w
                in
                span []
                    [ a
                        [ href url
                        , target "_blank"
                        , attribute "rel" "noopener noreferrer"
                        ]
                        [ text url ]
                    , text trailing
                    ]

            else
                text w

        renderLine line =
            if String.isEmpty line then
                [ text "\n" ]

            else
                (line
                    |> String.words
                    |> List.map renderWord
                    |> List.intersperse (text " ")
                )
                    ++ [ text "\n" ]
    in
    desc
        |> String.lines
        |> List.concatMap renderLine


{-| Format a yt-dlp upload\_date string (YYYYMMDD) as YYYY-MM-DD.
-}
formatDate : String -> String
formatDate date =
    if String.length date == 8 then
        String.slice 0 4 date
            ++ "-"
            ++ String.slice 4 6 date
            ++ "-"
            ++ String.slice 6 8 date

    else
        date


formatViewCount : Int -> String
formatViewCount n =
    if n >= 1000000 then
        String.fromFloat (toFloat (n // 100000) / 10) ++ "M"

    else if n >= 1000 then
        String.fromFloat (toFloat (n // 100) / 10) ++ "K"

    else
        String.fromInt n


formatDuration : Float -> String
formatDuration secs =
    let
        total =
            round secs

        hours =
            total // 3600

        minutes =
            (total - hours * 3600) // 60

        seconds =
            total - hours * 3600 - minutes * 60

        pad n =
            if n < 10 then
                "0" ++ String.fromInt n

            else
                String.fromInt n
    in
    if hours > 0 then
        String.fromInt hours ++ ":" ++ pad minutes ++ ":" ++ pad seconds

    else
        String.fromInt minutes ++ ":" ++ pad seconds


mediaErrorDecoder : D.Decoder MediaErrorCode
mediaErrorDecoder =
    D.at [ "target", "error", "code" ] D.int
        |> D.map
            (\code ->
                case code of
                    1 ->
                        ErrAborted

                    2 ->
                        ErrNetwork

                    3 ->
                        ErrDecode

                    4 ->
                        ErrSrcNotSupported

                    _ ->
                        ErrUnknown code
            )


mediaErrorMessage : MediaErrorCode -> String
mediaErrorMessage code =
    case code of
        ErrAborted ->
            "Playback was aborted."

        ErrNetwork ->
            "A network error prevented the video from loading."

        ErrDecode ->
            "The video could not be decoded (codec error)."

        ErrSrcNotSupported ->
            "This video format is not supported by your browser (AV1/WebM)."

        ErrUnknown n ->
            "Playback failed (error code " ++ String.fromInt n ++ ")."
